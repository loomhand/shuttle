import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test, type TestContext } from 'node:test'

import { daemon, notepad, user } from './daemon.mts'
import type { Call, W } from './daemon.mts'

type Lib = typeof import('../lib/gui_plan.mts')
type Plan = typeof auto

const NOTE = 'unverifiable: Sent 2 char(s) via PostMessage (not verified).'
const TTY = 'unverifiable: Sent (unverified) 3 char(s) via CGEvent (terminal emulator, 0ms delay). — a terminal may not echo what it reads (a password prompt), so neither the screen nor a screenshot can confirm it; do not type the text again.'
const LONG = `${'$ ls\nREADME.md\n'.repeat(20)}$ `
const READ = 64 << 10
const HUGE = LONG.repeat(250)
const tools = (calls: Call[]) => calls.map((c) => `${c.tool} ${JSON.stringify(c.args)}`)
const load = async (input: string): Promise<Lib['main']> => {
  process.env.SHUTTLE_INPUT = input
  return ((await import(`../lib/gui_plan.mts?${input}`)) as Lib).main
}
const background = await load('background')
const auto = await load('auto')
const modes = [['background', background], ['auto', auto]] as const

function desktop(typed: (before: string, text: string) => string, note: boolean) {
  const ws = [user(), notepad()]
  let doc = 'x'
  const calls: Call[] = []
  const reply = (c: Call) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify(ws)
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes: [{ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: doc }], truncated: false })
    if (c.tool === 'desk_focus') { ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: true }; return JSON.stringify({ ok: true, title: ws[1].title, pid: 4 }) }
    if (c.tool === 'desk_type') {
      if (c.args.delivery_mode === 'foreground') { doc += String(c.args.text); return JSON.stringify({ ok: true, chars: 2 }) }
      doc = typed(doc, String(c.args.text))
      return JSON.stringify({ ok: true, chars: 2, ...(note ? { unverified: NOTE } : {}) })
    }
    return '{"ok":true}'
  }
  return { calls, reply, doc: () => doc }
}

async function run(t: TestContext, plan: Plan, typed: (before: string, text: string) => string, note: boolean) {
  const d = desktop(typed, note)
  await daemon(t, d.reply)
  const r = await plan({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  return { r, calls: d.calls, doc: d.doc() }
}

async function terminal(t: TestContext, plan: Plan, typed: (screen: string, text: string) => string, { screen = '$ read -s', type = 'abc', sure = (_: string): boolean => false, key = '', prompt = '', secret = false } = {}) {
  const term: W = { id: 't', title: 'Terminal', role: 'window', pid: 7, active: false, app: 'Terminal' }
  const node = () => ({ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: screen })
  const calls: Call[] = []; let late = ''
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), term])
    if (c.tool === 'desk_snapshot') { const out = JSON.stringify({ window: term, nodes: [{ ...node(), value: screen.slice(0, 200) }], truncated: false }); screen += late; late = ''; return out }
    if (c.tool === 'desk_read') return JSON.stringify({ ...node(), value: c.args.tail ? screen.slice(-READ) : screen.slice(0, READ), ...(screen.length > READ ? { truncated: true } : {}) })
    if (c.tool === 'desk_key') { screen += '\n$ '; late = prompt; return JSON.stringify({ ok: true, ...(key ? { unverified: key } : {}) }) }
    if (c.tool !== 'desk_type') return '{"ok":true}'
    const text = String(c.args.text)
    screen = typed(screen, text)
    return JSON.stringify({ ok: true, chars: 3, terminal: true, ...(sure(text) ? {} : { unverified: TTY }) })
  })
  const step = secret ? { type, secret } : { type }
  const steps = type.includes('\n') ? [{ app: 'Terminal' }, step] : [{ app: 'Terminal' }, step, { key: 'Return' }]
  return { r: await plan({ steps }), calls }
}

test('an auto plan sends the same desk calls as a background plan when nothing falls back', async (t) => {
  t.mock.timers.enable({ apis: ['Date'], now: Date.now() })
  for (const [typed, note] of [[(b: string, s: string) => b + s, false], [(b: string, s: string) => b + s, true]] as const) {
    const bg = await run(t, background, typed, note)
    const au = await run(t, auto, typed, note)
    assert.equal(bg.r.ok, true, JSON.stringify(bg.r))
    assert.equal(au.r.ok, true, JSON.stringify(au.r))
    assert.deepEqual(tools(au.calls), tools(bg.calls))
  }
})

test('typing that did not land and left the front alone runs once in the foreground under auto, and escalates under background', async (t) => {
  const bg = await run(t, background, (b) => b, true)
  assert.equal(bg.r.ok, false)
  assert.match(String(bg.r.escalate?.reason), /did not show/)
  const au = await run(t, auto, (b) => b, true)
  assert.equal(au.r.ok, true, JSON.stringify(au.r))
  assert.equal(au.doc, 'xhi')
  assert.deepEqual(au.calls.filter((c) => c.tool === 'desk_focus').map((c) => c.args), [{ window: 'Untitled - Notepad', pid: 4 }])
  assert.equal(au.r.trace?.[1]?.how, 'foreground')
})

test('typing that landed in part is never typed again in the foreground', async (t) => {
  const au = await run(t, auto, (b) => `${b}h`, true)
  assert.equal(au.r.ok, false)
  assert.match(String(au.r.escalate?.reason), /did not show/)
  assert.equal(au.doc, 'xh')
  assert.ok(!au.calls.some((c) => c.tool === 'desk_focus'))
})

test('after auto ran a step in the foreground, a dialog of the app in front fails no later step', async (t) => {
  const ws: (W & { owner?: string })[] = [user(), notepad()]
  const calls: Call[] = []
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify(ws)
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes: [], truncated: false })
    if (c.tool === 'desk_key') {
      ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: false }
      ws.push({ id: 's', title: 'Save as', role: 'dialog', pid: 4, active: true, owner: '1' })
      return JSON.stringify({ ok: true, input: 'foreground' })
    }
    return JSON.stringify({ ok: true, chars: 4 })
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { key: 'ctrl+shift+s' }, { type: 'name' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.trace?.[1]?.input, 'foreground')
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args), [{ text: 'name', delivery_mode: 'foreground' }])
})

test('an auto macOS menu step fronts the plan window and runs the entry in the foreground, where background refuses', { skip: process.platform !== 'darwin' }, async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: 'Untitled - Notepad', states: [] },
    { ref: 1, id: 'mb', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 2, id: 'm', parent: 1, depth: 2, role: 'menu', name: 'Format', states: ['showing'], bounds: [10, 0, 40, 20] },
    { ref: 3, id: 'f', parent: 2, depth: 3, role: 'menuitem', name: 'Font', states: ['showing'], bounds: [10, 30, 80, 20] },
  ]
  const plan = async (main: Plan) => {
    const ws = [user(), notepad()]
    const calls: Call[] = []
    await daemon(t, (c) => {
      calls.push(c)
      if (c.tool === 'desk_windows') return JSON.stringify(ws)
      if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes, truncated: false })
      if (c.tool === 'desk_focus') { ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: true }; return JSON.stringify({ ok: true, title: ws[1].title, pid: 4 }) }
      if (c.tool === 'desk_press') return JSON.stringify({ ok: true, how: 'action' })
      return '{"ok":true}'
    })
    return { r: await main({ steps: [{ app: 'Notepad' }, { menu: 'Format > Font' }] }), calls }
  }
  const bg = await plan(background)
  assert.equal(bg.r.ok, false)
  assert.match(String(bg.r.escalate?.reason), /menu bar belongs to the front app/)
  const au = await plan(auto)
  assert.equal(au.r.ok, true, JSON.stringify(au.r))
  assert.deepEqual(au.calls.filter((c) => c.tool === 'desk_focus').map((c) => c.args), [{ window: 'Untitled - Notepad', pid: 4 }])
  assert.deepEqual(au.calls.filter((c) => c.tool === 'desk_press').map((c) => c.args), [{ id: 'f' }])
  assert.equal(au.r.trace?.[1]?.input, 'foreground')
})

test('a field auto cannot read is never typed into again in the foreground', async (t) => {
  const calls: Call[] = []
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [{ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [] }], truncated: false })
    if (c.tool === 'desk_read') return { text: 'the element answers no text', error: true }
    if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: 2, unverified: NOTE })
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /did not show/)
  assert.ok(!calls.some((c) => c.tool === 'desk_focus'))
  assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1)
})

test('a menu state that needs pixels is refused in the background under auto before any menu opens', { skip: process.platform === 'darwin' }, async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: 'Untitled - Notepad', states: [] },
    { ref: 1, id: 'mb', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 2, id: 'm', parent: 1, depth: 2, role: 'menu', name: 'View', states: ['showing'], bounds: [10, 0, 40, 20] },
    { ref: 3, id: 's', parent: 2, depth: 3, role: 'menuitem', name: 'Status bar', states: ['showing'], bounds: [10, 30, 80, 20] },
  ]
  const calls: Call[] = []
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes, truncated: false })
    if (c.tool === 'desk_press') return JSON.stringify({ ok: true, how: 'action' })
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { menu: 'View > Status bar', on: true }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /does not read the screen/)
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_press'), [])
  assert.ok(!calls.some((c) => c.tool === 'desk_focus' || c.tool === 'desk_shot'))
})

test('a window that shows auto no field to read is never typed into again in the foreground', async (t) => {
  const calls: Call[] = []
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [{ ref: 0, id: 'w', parent: -1, depth: 0, role: 'window', name: 'Untitled - Notepad', states: [] }, { ref: 1, id: 'b', parent: 0, depth: 1, role: 'button', name: 'Send', states: ['showing'] }], truncated: false })
    if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: 2, unverified: NOTE })
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /did not show/)
  assert.ok(!calls.some((c) => c.tool === 'desk_focus'))
  assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1)
})

test('a background note left open before a later step took the foreground still fails the plan', async (t) => {
  await daemon(t, (c) => {
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [], truncated: false })
    if (c.tool === 'desk_key' && c.args.keys === 'ctrl+b') return JSON.stringify({ ok: true, unverified: 'unverifiable: posted ctrl+b' })
    if (c.tool === 'desk_key') return JSON.stringify({ ok: true, input: 'foreground' })
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { key: 'ctrl+b' }, { key: 'ctrl+a' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.step, 1)
  assert.match(String(r.escalate?.reason), /not confirmed.*posted ctrl\+b/)
  assert.deepEqual(r.unverified, [{ step: 1, note: 'unverifiable: posted ctrl+b' }])
})

test('a background step that fell back to the foreground leaves the end of the plan to list the windows again', async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: notepad().title, states: ['showing'] },
    { ref: 1, id: 's', parent: 0, depth: 1, role: 'button', name: 'Save', states: ['showing'] },
  ]
  const calls: Call[] = []
  await daemon(t, (c) => {
    if (!c.args.waiting) calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes, truncated: false })
    if (c.tool === 'desk_press') return JSON.stringify({ ok: true, how: 'click', input: 'foreground' })
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { click: 'Save' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  const pressed = calls.findIndex((c) => c.tool === 'desk_press')
  assert.ok(pressed >= 0, tools(calls).join('\n'))
  assert.equal(calls.slice(pressed + 1).filter((c) => c.tool === 'desk_windows').length, 3, tools(calls).join('\n'))
})

test('a password auto cannot see in its field is never typed again in the foreground', async (t) => {
  const calls: Call[] = []
  const field = { ref: 1, id: 'p', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'focused', 'protected'] }
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [{ ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: notepad().title, states: [] }, field], truncated: false })
    if (c.tool === 'desk_read') return JSON.stringify({ ...field, value: '' })
    if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: 7, unverified: NOTE })
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { type: '{{password}}', focus: { secure: true } }], params: { password: 'hunter2' } })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(String(r.escalate?.reason), /^the password did not show/)
  assert.ok(!calls.some((c) => c.tool === 'desk_focus'))
  assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1)
})

test('typing a terminal does not show is never typed again in the foreground, and the plan goes on with the step unverified', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen) => screen)
    assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1, `${mode}\n${tools(calls).join('\n')}`)
    assert.ok(!calls.some((c) => c.tool === 'desk_focus'), mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], mode)
    assert.equal(r.escalate?.step, 1, mode)
    assert.match(String(r.escalate?.reason), /not confirmed/, mode)
  }
})

test('typing a terminal shows in part stops the plan before its next key', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text.slice(0, 2))
    assert.equal(r.ok, false, mode)
    assert.equal(r.escalate?.step, 1, mode)
    assert.match(String(r.escalate?.reason), /did not show/, mode)
    assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1, mode)
    assert.ok(!calls.some((c) => c.tool === 'desk_key' || c.tool === 'desk_focus'), `${mode}\n${tools(calls).join('\n')}`)
  }
})

test('typing an echoing terminal shows is confirmed', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text)
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r)}`)
    assert.equal(r.unverified, undefined, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
  }
})

test('a secret step names the password, never its text, in a stop and in the saved chain', async (t) => {
  for (const [mode, plan] of modes) {
    for (const secret of [true, false]) {
      const stop = await terminal(t, plan, (screen, text) => screen + (text === 'example-pass' ? 'exam' : text), { type: 'read -s pw\nexample-pass\n', secret })
      assert.match(String(stop.r.escalate?.reason), secret ? /^the password did not show/ : /^"read -s pw\nexample-pass\n" did not show/, `${mode} ${secret}`)
      assert.equal(JSON.stringify(stop.r).includes('example-pass'), !secret, `${mode} ${secret} result`)
      await terminal(t, plan, (screen, text) => screen + text, { type: 'read -s pw\nexample-pass\n', secret })
      assert.equal(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8').includes('example-pass'), !secret, `${mode} ${secret} chain`)
    }
  }
})

test('terminal confirmation takes a typed trailing space or tab the terminal trimmed', async (t) => {
  for (const [mode, plan] of modes) {
    for (const type of ['ls ', 'ls\t', 'ls \n', 'ls\t\n']) {
      const { r, calls } = await terminal(t, plan, (screen, text) => screen + text.trimEnd(), { type })
      assert.equal(r.ok, true, `${mode} ${JSON.stringify(type)} ${JSON.stringify(r.escalate?.reason)}`)
      assert.equal(r.unverified, undefined, `${mode} ${JSON.stringify(type)}`)
      assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], `${mode} ${JSON.stringify(type)}`)
    }
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text.trimEnd(), { type: 'ls \npwd\n' })
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r.escalate?.reason)}`)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return', 'Return'], mode)
  }
})

test('terminal confirmation takes echoed whitespace-only text and lines', async (t) => {
  for (const [mode, plan] of modes) {
    for (const [type, returns] of [[' ', 1], ['\t', 1], ['ls\n    \npwd\n', 3], ['  \nls\n', 2]] as const) {
      const { r, calls } = await terminal(t, plan, (screen, text) => screen + text, { type, sure: () => false })
      assert.doesNotMatch(String(r.escalate?.reason), /did not show/, `${mode} ${JSON.stringify(type)}`)
      assert.equal(calls.filter((c) => c.tool === 'desk_key').length, returns, `${mode} ${JSON.stringify(type)}`)
    }
  }
})

test('terminal confirmation takes a line break the terminal wrapped into the text', async (t) => {
  for (const [mode, plan] of modes) {
    for (const type of ['echo hello', 'echo hello\n']) {
      const { r, calls } = await terminal(t, plan, (screen, text) => text === 'echo hello' ? `${screen}echo hel\nlo` : screen + text, { type })
      assert.doesNotMatch(String(r.escalate?.reason), /did not show/, `${mode} ${type}`)
      assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], `${mode} ${type}`)
    }
  }
})

test('terminal confirmation preserves spaces and case before sending Return', async (t) => {
  for (const [mode, plan] of modes) {
    for (const type of ['echo hello', 'echo hello\n']) {
      for (const observed of ['echohello', 'echo HELLO']) {
        const { r, calls } = await terminal(t, plan, (screen) => screen + observed, { type })
        assert.equal(r.ok, false, `${mode} ${type} ${observed}`)
        assert.match(String(r.escalate?.reason), /did not show/)
        assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1)
        assert.ok(!calls.some((c) => c.tool === 'desk_key' || c.tool === 'desk_focus'), tools(calls).join('\n'))
      }
    }
  }
})

test('typing a long terminal screen shows in part at its end stops the plan before its next key', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text.slice(0, 2), { screen: LONG })
    assert.equal(r.ok, false, mode)
    assert.match(String(r.escalate?.reason), /did not show/, mode)
    assert.ok(!calls.some((c) => c.tool === 'desk_key' || c.tool === 'desk_focus'), `${mode}\n${tools(calls).join('\n')}`)
  }
})

test('typing a long terminal screen does not show goes on unverified', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen) => screen, { screen: LONG })
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], mode)
  }
})

test('typing a terminal screen past the read limit shows in part at its end stops the plan before its next key', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text.slice(0, 2), { screen: HUGE })
    assert.equal(r.ok, false, mode)
    assert.match(String(r.escalate?.reason), /did not show/, mode)
    assert.ok(!calls.some((c) => c.tool === 'desk_key' || c.tool === 'desk_focus'), `${mode}\n${tools(calls).join('\n')}`)
  }
})

test('typing a terminal screen past the read limit does not show goes on unverified', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen) => screen, { screen: HUGE })
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], `${mode} ${JSON.stringify(r.escalate?.reason)}`)
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], mode)
  }
})

test('typing a terminal screen past the read limit shows at its end is confirmed', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text, { screen: HUGE })
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r.escalate?.reason)}`)
    assert.equal(r.unverified, undefined, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
  }
})

test('typing a long terminal screen shows at its end is confirmed', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text, { screen: LONG })
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r)}`)
    assert.equal(r.unverified, undefined, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
  }
})

test('a typed line a terminal shows in part gets no Return', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text.slice(0, 7), { type: 'echo hello\n' })
    assert.equal(r.ok, false, mode)
    assert.match(String(r.escalate?.reason), /did not show/, mode)
    assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1, mode)
    assert.ok(!calls.some((c) => c.tool === 'desk_key' || c.tool === 'desk_focus'), `${mode}\n${tools(calls).join('\n')}`)
  }
})

test('a typed line a terminal does not echo is submitted with one Return and stays unverified', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen) => screen, { type: 'pass\n' })
    assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1, `${mode}\n${tools(calls).join('\n')}`)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
    assert.ok(!calls.some((c) => c.tool === 'desk_focus'), mode)
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], `${mode} ${JSON.stringify(r)}`)
  }
})

test('a long field whose cut tail did not change is never typed into again in the foreground', async (t) => {
  let doc = `x${HUGE}`
  const calls: Call[] = []
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    const node = { ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: doc.slice(0, 200) }
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [node], truncated: false })
    if (c.tool === 'desk_read') return JSON.stringify({ ...node, value: doc.slice(-READ), truncated: true })
    if (c.tool === 'desk_type') { doc = `${doc.slice(0, 1)}${String(c.args.text)}${doc.slice(1)}`; return JSON.stringify({ ok: true, chars: 2, unverified: NOTE }) }
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.equal(r.ok, false, JSON.stringify(r.trace))
  assert.match(String(r.escalate?.reason), /did not show/)
  assert.ok(!calls.some((c) => c.tool === 'desk_focus'), tools(calls).join('\n'))
  assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1)
})

test('a background type of several lines sends each Return once when a terminal line that does not echo leaves its own screen unchanged', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => text === 'pass' ? screen : screen + text, { type: 'read -s pw\npass\n' })
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text), ['read -s pw', 'pass'], `${mode} ${JSON.stringify(r.escalate?.reason)}`)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return', 'Return'], mode)
    assert.ok(!calls.some((c) => c.tool === 'desk_focus'), mode)
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], mode)
  }
})

test('a background type of several lines into a window that is not a terminal sends the lines after the first in one call with no read between', async (t) => {
  for (const [mode, plan] of modes) {
    const d = desktop((b, s) => b + s, false)
    await daemon(t, d.reply)
    const r = await plan({ steps: [{ app: 'Notepad' }, { type: 'one\ntwo\nthree' }] })
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r)}`)
    const typed = d.calls.findIndex((c) => c.tool === 'desk_type')
    assert.deepEqual(d.calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text), ['one', '\ntwo\nthree'], mode)
    assert.deepEqual(d.calls.slice(typed + 1, typed + 2).map((c) => c.tool), ['desk_type'], `${mode}\n${tools(d.calls).join('\n')}`)
    assert.ok(!d.calls.some((c) => c.tool === 'desk_key'), mode)
  }
})

test('a background type of several lines into a terminal types each line against its own read-back', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text, { type: 'ls\npwd\n', sure: () => true })
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r.escalate?.reason)}`)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text), ['ls', 'pwd'], mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return', 'Return'], mode)
  }
})

test('a terminal line waits for a prompt drawn after its Return before it is checked', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => text === 'pass' ? screen : screen + text, { type: 'read -s pw\npass\n', prompt: 'Password: ' })
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return', 'Return'], `${mode} ${JSON.stringify(r.escalate?.reason)}`)
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], mode)
  }
})

test('a terminal step whose lines the driver confirmed is confirmed though a Return between them was not', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text, { type: 'ls\npwd\n', sure: () => true, key: 'unverifiable: posted return' })
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r.escalate?.reason)}`)
    assert.equal(r.unverified, undefined, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return', 'Return'], mode)
  }
})

test('a terminal note on a later line replaces a Return note the step heard before it', async (t) => {
  for (const [mode, plan] of modes) {
    const { r } = await terminal(t, plan, (screen, text) => text === 'pass' ? screen : screen + text, { type: 'read -s pw\npass\n', sure: (x) => x !== 'pass', key: 'unverifiable: posted return' })
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], `${mode} ${JSON.stringify(r)}`)
  }
})

test('a terminal line that did not show stops the plan and says which line and what went before it', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => text === 'pwd' ? `${screen}pw` : screen + text, { type: 'ls\npwd\n' })
    assert.match(String(r.escalate?.reason), /did not show in "Terminal" after typing line 2 of 2, after the line before it and its Return were sent/, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
  }
})

test('a blank answer before a quiet line is submitted alone and the quiet line is checked against the screen after it', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + text.replace('secret', '').replace(/\n/g, '\n$ '), { type: '\nsecret\n' })
    assert.doesNotMatch(String(r.escalate?.reason), /did not show/, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text), ['\n', 'secret'], mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], mode)
  }
})

test('a CRLF text into a terminal is typed line by line and a partial line stops the plan', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen, text) => screen + (text.startsWith('pass') ? 'pa' : text), { type: '\r\npass\r\n' })
    assert.match(String(r.escalate?.reason), /did not show in "Terminal" after typing line 2 of 2, after the line before it and its Return were sent/, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text), ['\n', 'pass\r'], mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key'), [], mode)
  }
})

test('leading blank lines go through the desk type that checks the focus of the last click', async (t) => {
  const REFUSED = 'type refused, nothing was typed: the keyboard focus is in textbox "Search", not in the field of the last click into this window, so the text would go there; click the field to type into, then type again'
  for (const [mode, plan] of modes) {
    const ws = [user(), notepad()]; const calls: Call[] = []; let clicked = true
    await daemon(t, (c) => {
      calls.push(c)
      if (c.tool === 'desk_windows') return JSON.stringify(ws)
      if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes: [{ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: 'x' }], truncated: false })
      if (c.tool === 'desk_type' && clicked) return { text: REFUSED, error: true }
      if (c.tool !== 'desk_type' && c.tool.startsWith('desk_') && !['desk_windows', 'desk_snapshot', 'desk_read', 'desk_wait'].includes(c.tool)) clicked = false
      return '{"ok":true}'
    })
    const r = await plan({ steps: [{ app: 'Notepad' }, { type: '\nhello' }] })
    assert.equal(r.ok, false, mode)
    assert.match(JSON.stringify(r), /type refused, nothing was typed/, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key'), [], mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text), ['\n'], mode)
  }
})

test('a terminal line that did not show keeps the step unverified though a later line showed', async (t) => {
  for (const [mode, plan] of modes) {
    const { r } = await terminal(t, plan, (screen, text) => text === 'pass' ? screen : screen + text, { type: 'pass\necho hi\n' })
    assert.deepEqual(r.unverified, [{ step: 1, note: TTY }], `${mode} ${JSON.stringify(r)}`)
  }
})

test('a long field read in part before or after typing is never typed into again in the foreground', async (t) => {
  for (const cut of [0, 1]) {
    const doc = HUGE
    const calls: Call[] = []
    let reads = 0
    await daemon(t, (c) => {
      calls.push(c)
      if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
      const node = { ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: doc.slice(0, 200) }
      if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [node], truncated: false })
      if (c.tool === 'desk_read') return JSON.stringify({ ...node, value: doc.slice(-READ), ...(reads++ === cut ? { truncated: true } : {}) })
      if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: 2, unverified: NOTE })
      return '{"ok":true}'
    })
    const r = await auto({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
    assert.equal(r.ok, false, `${cut} ${JSON.stringify(r.trace)}`)
    assert.ok(!calls.some((c) => c.tool === 'desk_focus'), `${cut}\n${tools(calls).join('\n')}`)
  }
})

test('a step keeps the first note it heard unless a later one names a terminal', async (t) => {
  await daemon(t, (c) => {
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [{ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: 'x' }], truncated: false })
    if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: 3, unverified: `unverifiable: ${String(c.args.text).trim()}` })
    return '{"ok":true}'
  })
  const r = await background({ steps: [{ app: 'Notepad' }, { type: 'one\ntwo' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(String(r.escalate?.reason), /did not show in "Untitled - Notepad" after typing: unverifiable: one$/)
})
