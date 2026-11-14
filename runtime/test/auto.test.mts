import assert from 'node:assert/strict'
import { readdirSync, readFileSync } from 'node:fs'
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
const pressed = (calls: Call[]) => calls.filter((c) => c.tool === 'desk_key').flatMap((c) => Array<unknown>(Number(c.args.repeat ?? 1)).fill(c.args.keys))
const load = async (input: string): Promise<Lib['main']> => {
  process.env.SHUTTLE_INPUT = input
  return ((await import(`../lib/gui_plan.mts?${input}`)) as Lib).main
}
const background = await load('background')
const auto = await load('auto')
const foreground = await load('foreground')
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

async function terminal(t: TestContext, plan: Plan, typed: (screen: string, text: string) => string, { screen = '$ read -s', type = 'abc', sure = (_: string): boolean => false, key = '', prompt = '', extra = {} as Record<string, unknown>, front = false, answer = (s: string) => `${s}\n$ `, blind = false, named = true, role = 'document', erases = true, eraseFails = false, eraseBlinds = false, readBlinds = false, steal = false, stealMid = false, eraseLimit = Infinity } = {}) {
  const term: W = { id: 't', title: 'Terminal', role: 'window', pid: 7, active: front, app: 'Terminal' }
  const bare = role === 'text' || role === 'terminal'
  const node = () => ({ ref: 0, id: 'd', parent: -1, depth: 0, role, name: role === 'text' ? 'Command Prompt' : '', states: role === 'text' ? ['focused'] : [], ...(bare ? {} : { value: screen }) })
  const calls: Call[] = []; let late = ''; let keyed = false; let erased = false; let typed1 = false; let backs = 0
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify(front && !(steal && typed1) && !(stealMid && backs > 0) ? [{ ...user(), active: false }, term] : front ? [user(), { ...term, active: false }] : [user(), term])
    if (((blind && keyed) || (eraseBlinds && erased)) && (c.tool === 'desk_snapshot' || c.tool === 'desk_read')) throw new Error('the window answers no accessibility calls')
    if (readBlinds && erased && c.tool === 'desk_read') throw new Error('the node answers no text')
    if (c.tool === 'desk_snapshot') { const out = JSON.stringify({ window: term, nodes: [{ ...node(), ...(bare ? {} : { value: screen.slice(0, 200) }) }], truncated: false }); screen += late; late = ''; return out }
    if (c.tool === 'desk_read') return JSON.stringify({ ...node(), value: c.args.tail ? screen.slice(-READ) : screen.slice(0, READ), ...(screen.length > READ ? { truncated: true } : {}) })
    if (c.tool === 'desk_key' && c.args.keys === 'backspace') { if (eraseFails) throw new Error('bring_to_front: refused'); erased = true; for (let n = Number(c.args.repeat ?? 1); n > 0; n--) { backs += 1; if (erases && backs <= eraseLimit) screen = screen.slice(0, -1) } return '{"ok":true}' }
    if (c.tool === 'desk_key') { screen = answer(screen); late = prompt; keyed = true; return JSON.stringify({ ok: true, ...(key ? { unverified: key } : {}) }) }
    if (c.tool !== 'desk_type') return '{"ok":true}'
    const text = String(c.args.text)
    typed1 = true
    screen = typed(screen, text)
    return JSON.stringify(front ? { ok: true, chars: 3 } : named ? { ok: true, chars: 3, terminal: true, ...(sure(text) ? {} : { unverified: TTY }) } : { ok: true, chars: 3, unverified: 'unverifiable: Sent 3 char(s) via PostMessage (not verified).' })
  })
  const step = { type, ...extra }
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

test('a secret step never shows its text, and a password the terminal echoed stops before its Return', async (t) => {
  const hidden = (r: unknown) => !JSON.stringify(r).includes('example-pass') && !JSON.stringify(r).includes('exam')
  const kept = () => readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8').includes('exam')
  for (const [mode, plan] of modes) {
    for (const type of ['example-pass', 'example-pass\n']) {
      const part = await terminal(t, plan, (screen, text) => screen + (text === 'example-pass' ? 'exam' : text), { type, extra: { secret: true } })
      assert.match(String(part.r.escalate?.reason), /^"Terminal" changed while the password was typed, and nothing showed that a prompt took it, so it was not submitted: /, `${mode} ${JSON.stringify(type)}`)
      assert.equal(part.r.escalate?.outline, undefined, `${mode} ${JSON.stringify(type)}`)
      assert.ok(hidden(part.r), `${mode} ${JSON.stringify(type)} ${JSON.stringify(part.r)}`)
      for (const sure of [(_: string) => false, (x: string) => x === 'example-pass']) {
        const echo = await terminal(t, plan, (screen, text) => screen + text, { type, extra: { secret: true }, sure })
        assert.match(String(echo.r.escalate?.reason), /^the password showed in "Terminal", so it was not submitted, and it was erased$/, `${mode} ${JSON.stringify(type)}`)
        assert.deepEqual(pressed(echo.calls), Array(12).fill('backspace'), `${mode} ${JSON.stringify(type)}`)
        assert.ok(hidden(echo.r), `${mode} ${JSON.stringify(type)}`)
      }
    }
    assert.equal(kept(), false, mode)
  }
})

test('a background secret password line counts as landed when the terminal moves after its Return', async (t) => {
  for (const [mode, plan] of modes) {
    const { r, calls } = await terminal(t, plan, (screen) => screen, { type: 'example-pass\n', extra: { secret: true } })
    assert.equal(r.ok, true, `${mode} ${JSON.stringify(r)}`)
    assert.equal(r.unverified, undefined, mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text), ['example-pass'], mode)
    assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys), ['Return'], mode)
    assert.ok(!JSON.stringify(r).includes('example-pass'), mode)
  }
})

test('a background secret password line stays unconfirmed, with no outline, when the terminal does not move or cannot be read after its Return', async (t) => {
  for (const opts of [{ answer: (s: string) => s }, { blind: true }]) {
    const { r } = await terminal(t, background, (screen) => screen, { type: 'example-pass\n', extra: { secret: true }, ...opts })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(String(r.escalate?.reason), /was sent but not confirmed/, JSON.stringify(opts))
    assert.equal(r.escalate?.outline, undefined, JSON.stringify(r.escalate))
    assert.ok(!JSON.stringify(r).includes('example-pass'), JSON.stringify(r))
  }
})

test('an unconfirmed secret step reports a fixed note that gives neither the password\'s length nor its keys', async (t) => {
  const { r } = await terminal(t, background, (screen) => screen, { type: 'example-pass\n', extra: { secret: true }, answer: (s: string) => s })
  const notes = JSON.stringify([r.escalate?.reason, r.unverified])
  assert.ok(notes.includes('the password was sent and the target did not confirm it (terminal emulator)'), notes)
  assert.ok(!notes.includes('char(s)') && !notes.includes('example-pass'), notes)
})

test('a background secret password line that shows only after its Return stops instead of counting as landed', async (t) => {
  const { r } = await terminal(t, background, (screen) => screen, { type: 'example-pass\n', extra: { secret: true }, answer: (s: string) => `${s}example-pass\n$ ` })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, 'the password showed in "Terminal" after its Return was sent; clear it (in a terminal, for example `clear`) before any screenshot')
  assert.equal(r.escalate?.outline, undefined)
  assert.ok(!JSON.stringify(r).includes('example-pass'), JSON.stringify(r))
})

test('a background secret line whose reply names no terminal stops when the password shows only after its Return', async (t) => {
  const typed = (screen: string, text: string) => text.startsWith('\n') ? `${screen}example-pass\n$ ` : screen
  const { r } = await terminal(t, background, typed, { type: 'example-pass\n', extra: { secret: true }, named: false })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, 'the password showed in "Terminal" after its Return was sent; clear it (in a terminal, for example `clear`) before any screenshot')
  assert.equal(r.escalate?.outline, undefined)
  assert.ok(!JSON.stringify(r).includes('example-pass'), JSON.stringify(r))
})

test('typing a Windows Terminal does not change is never typed again in the foreground, as its terminal text counts as no field', async (t) => {
  const { calls } = await terminal(t, auto, (screen) => screen, { type: 'j', named: false, role: 'text' })
  assert.ok(!calls.some((c) => c.tool === 'desk_focus'), JSON.stringify(calls.map((c) => c.tool)))
  assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1)
})

test('a background secret line whose reply names no terminal, as on Windows, stops before its Return when the password shows', async (t) => {
  for (const role of ['document', 'text']) await unnamed(t, role)
})

async function unnamed(t: TestContext, role: string) {
  const keys = (calls: Call[]) => calls.filter((c) => (c.tool === 'desk_key' && c.args.keys !== 'backspace') || (c.tool === 'desk_type' && String(c.args.text).startsWith('\n')))
  const echo = await terminal(t, background, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, named: false, role })
  assert.equal(echo.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it was erased')
  assert.deepEqual(keys(echo.calls), [])
  assert.ok(!JSON.stringify(echo.r).includes('example-pass'), JSON.stringify(echo.r))
  const quiet = await terminal(t, background, (screen, text) => text === 'example-pass' ? screen : screen + text, { type: 'example-pass\n', extra: { secret: true }, named: false, role })
  assert.ok(!String(quiet.r.escalate?.reason ?? '').includes('the password showed'), JSON.stringify(quiet.r))
  assert.equal(keys(quiet.calls).length, 1, JSON.stringify(quiet.calls.map((c) => c.tool)))
}

test('a foreground secret step reads a valueless terminal node back, as xfce4-terminal shows one, and stops before its Return when the password shows', async (t) => {
  const keys = (calls: Call[]) => calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys)
  const echo = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, role: 'terminal' })
  assert.equal(echo.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it was erased')
  assert.deepEqual(keys(echo.calls), Array(12).fill('backspace'))
  assert.ok(!JSON.stringify(echo.r).includes('example-pass'), JSON.stringify(echo.r))
  const quiet = await terminal(t, foreground, (screen) => screen, { type: 'example-pass\n', extra: { secret: true }, front: true, role: 'terminal' })
  assert.equal(quiet.r.ok, true, JSON.stringify(quiet.r))
  assert.deepEqual(keys(quiet.calls), ['Return'])
})

test('a shown password counts as erased when text outside it, such as a prompt that showed after the base read, stays', async (t) => {
  const late = (screen: string, text: string) => text.startsWith('example-pass') ? `${screen}pwB: ${text}` : screen + text
  for (const [mode, plan, front] of [...modes.map(([m, p]) => [m, p, false] as const), ['foreground', foreground, true] as const]) {
    const { r, calls } = await terminal(t, plan, late, { type: 'example-pass\n', extra: { secret: true }, front })
    assert.equal(r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it was erased', mode)
    assert.deepEqual(pressed(calls), Array(12).fill('backspace'), mode)
    assert.equal(calls.filter((c) => c.tool === 'desk_key').length, front ? 12 : 1, mode)
  }
})

test('a shown password still stops with the secret reason when Backspace fails or the window cannot be read back', async (t) => {
  const failed = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, eraseFails: true })
  assert.equal(failed.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it could not be erased: clear it by hand')
  assert.equal(failed.r.escalate?.outline, undefined)
  assert.ok(!JSON.stringify(failed.r).includes('example-pass'), JSON.stringify(failed.r))
  const blind = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, eraseBlinds: true })
  assert.equal(blind.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, Backspace was sent over it, unchecked')
  assert.ok(!JSON.stringify(blind.r).includes('example-pass'), JSON.stringify(blind.r))
})

test('a shown password is erased only in its own front window, by the characters typed, and called erased only on a full read-back', async (t) => {
  const back = (calls: Call[]) => pressed(calls).filter((k) => k === 'backspace').length
  const stolen = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, steal: true })
  assert.equal(stolen.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it could not be erased: clear it by hand')
  assert.equal(back(stolen.calls), 0)
  const crlf = await terminal(t, foreground, (screen, text) => screen + text.replace(/\r$/, ''), { type: 'example-pass\r\n', extra: { secret: true }, front: true })
  assert.equal(back(crlf.calls), 12)
  const mid = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, stealMid: true })
  assert.equal(back(mid.calls), 1)
  assert.equal(mid.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it could not be erased: clear it by hand')
  const part = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, eraseLimit: 5 })
  assert.equal(part.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it could not be erased: clear it by hand')
  const unread = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, role: 'terminal', readBlinds: true })
  assert.equal(unread.r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, Backspace was sent over it, unchecked')
})

test('a shown password that Backspace does not erase is reported as still on screen', async (t) => {
  const { r } = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true, erases: false })
  assert.equal(r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it could not be erased: clear it by hand')
  assert.ok(!JSON.stringify(r).includes('example-pass'), JSON.stringify(r))
})

test('a foreground secret step reads the window back before its Return, as a foreground type reply names no terminal', async (t) => {
  const keys = (calls: Call[]) => calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.keys)
  const typed = (calls: Call[]) => calls.filter((c) => c.tool === 'desk_type').map((c) => c.args.text)
  for (const shown of [(screen: string) => screen, (screen: string) => `${screen}P: `]) {
    const { r, calls } = await terminal(t, foreground, shown, { type: 'example-pass\n', extra: { secret: true }, front: true })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.deepEqual(typed(calls), ['example-pass'])
    assert.deepEqual(keys(calls), ['Return'])
  }
  const { r, calls } = await terminal(t, foreground, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, front: true })
  assert.match(String(r.escalate?.reason), /^the password showed in "Terminal", so it was not submitted, and it was erased$/)
  assert.deepEqual(keys(calls), Array(12).fill('backspace'))
  assert.deepEqual(typed(calls), ['example-pass'])
  assert.ok(!JSON.stringify(r).includes('example-pass'), JSON.stringify(r))
})

test('a secret step that is not one password line, or whose secret is not a boolean, is refused', async (t) => {
  await daemon(t, () => '[]')
  for (const [step, why] of [
    [{ type: 5, secret: true }, 'type takes a string'],
    [{ type: 'read -s pw\nexample-pass\n', secret: true }, 'a secret step holds only the password line; put the command in its own type step'],
    [{ type: 'example-pass\n', secret: 'true' }, 'secret takes true or false'],
  ] as const) {
    const r = await auto({ steps: [{ app: 'Terminal' }, step as never] })
    assert.equal(r.escalate?.reason, `step 1: ${why}; nothing ran`, JSON.stringify(step))
    assert.ok(!JSON.stringify(r).includes('example-pass'), JSON.stringify(r))
  }
})

test('a secret step drops its intent wherever the step is shown or kept', async (t) => {
  for (const [mode, plan] of modes) {
    const { r } = await terminal(t, plan, (screen, text) => screen + (text === 'example-pass' ? 'exam' : text), { type: 'example-pass\n', extra: { secret: true, intent: 'type "example-pass"' } })
    assert.equal(r.escalate?.step, 1, mode)
    assert.equal(r.escalate?.plan_step?.intent, undefined, mode)
    assert.ok(!JSON.stringify(r).includes('example-pass'), `${mode} ${JSON.stringify(r)}`)
  }
  const d = desktop((b, x) => b + x, false)
  await daemon(t, d.reply)
  const kept = await auto({ steps: [{ app: 'Notepad' }, { type: 'example-pass', secret: true, intent: 'type "example-pass"' }] })
  assert.equal(kept.ok, true, JSON.stringify(kept))
  const chain = readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')
  assert.ok(chain.includes('"secret":true') && !chain.includes('example-pass'), chain)
  const flows = readdirSync(process.env.SHUTTLE_FLOWS ?? '').filter((f) => f.endsWith('.json') && !f.startsWith('.'))
  assert.ok(flows.length > 0, 'a passing plan saves its flow')
  for (const f of flows) assert.ok(!readFileSync(`${process.env.SHUTTLE_FLOWS}/${f}`, 'utf8').includes('example-pass'), f)
  await daemon(t, () => '[]')
  const two = await auto({ steps: [{ app: 'Terminal' }, { type: 'example-pass', secret: true, intent: 'type "example-pass"', key: 'x' } as never] })
  assert.ok(!JSON.stringify(two).includes('example-pass'), JSON.stringify(two))
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

test('a background or auto erase that leaves part of the password, or none of it, says it could not be erased', async (t) => {
  for (const [mode, plan] of modes) {
    for (const opts of [{ eraseLimit: 5 }, { erases: false }]) {
      const { r } = await terminal(t, plan, (screen, text) => screen + text, { type: 'example-pass\n', extra: { secret: true }, ...opts })
      assert.equal(r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it could not be erased: clear it by hand', `${mode} ${JSON.stringify(opts)}`)
      assert.ok(!JSON.stringify(r).includes('example-pass'), mode)
    }
  }
})

test('a password erased from a terminal read only in its tail counts as erased', async (t) => {
  for (const [mode, plan, front] of [...modes.map(([m, p]) => [m, p, false] as const), ['foreground', foreground, true] as const]) {
    const { r } = await terminal(t, plan, (screen, text) => screen + text, { screen: HUGE, type: 'example-pass\n', extra: { secret: true }, front })
    assert.equal(r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it was erased', mode)
  }
})

test('a background erase of a password longer than one repeat goes in calls of at most 256 presses', async (t) => {
  const long = `example-pass-${'0123456789'.repeat(30)}`
  const { r, calls } = await terminal(t, background, (screen, text) => screen + text, { type: `${long}\n`, extra: { secret: true } })
  assert.equal(r.escalate?.reason, 'the password showed in "Terminal", so it was not submitted, and it was erased')
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_key').map((c) => c.args.repeat), [256, long.length - 256])
})
