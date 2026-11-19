import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync, rmSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { test, type TestContext } from 'node:test'

import { daemon, gray, notepad, root, user } from './daemon.mts'
import type { Call, Reply, W } from './daemon.mts'
import { calls } from '../lib/desk.mts'

process.env.SHUTTLE_INPUT = 'background'
const { main } = await import('../lib/gui_plan.mts')

function desktop(ws: W[], answer: (c: Call) => Reply | undefined = () => undefined) {
  const calls: Call[] = []
  const reply = (c: Call): Reply => {
    calls.push(c)
    const own = answer(c)
    if (own !== undefined) return own
    if (c.tool === 'desk_windows') return JSON.stringify(ws)
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[0], nodes: [], truncated: false })
    if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: String(c.args.text).length })
    return '{"ok":true}'
  }
  return { calls, reply }
}

const sent = (calls: Call[], tool: string) => calls.filter((c) => c.tool === tool).map((c) => c.args)
const keysBefore = (calls: Call[], tool: string) => calls.slice(0, calls.findIndex((c) => c.tool === tool)).filter((c) => c.tool === 'desk_key').map((c) => c.args)

const TEXT = 'an untitled dialog of "R89Kit" is in front of "r89aN3" and takes all of its app\'s input until it closes, and a plan never presses a button of such a dialog: press its button with the desk tool (snapshot with the dialog\'s id from desk windows, then press with the button\'s id), or close it with {close: true} when it has a Cancel button; nothing was sent'
const SEVERAL = '2 untitled dialogs of "R89Kit" are open, and a plan never picks one of them: press the button meant with the desk tool (snapshot with that dialog\'s id from desk windows, then press with the button\'s id); nothing was sent'

test('a background plan types into its app window behind the front one, names it on every input and fronts nothing', async (t) => {
  const d = desktop([user(), notepad()])
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'hi' }, { key: 'ctrl+s' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'hi', window: 'Untitled - Notepad', pid: 4 }])
  assert.deepEqual(sent(d.calls, 'desk_key'), [{ keys: 'ctrl+s', window: 'Untitled - Notepad', pid: 4 }])
  for (const tool of ['desk_focus', 'desk_shot', 'desk_launch']) assert.ok(!d.calls.some((c) => c.tool === tool), tool)
})

test('a dialog the OS says the plan window owns becomes the target, and a window of the same app it does not own never does', async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') ws.push({ id: 's', title: 'Save as', role: 'dialog', pid: 4, active: false, owner: '1' }, { id: 'o', title: 'Other', role: 'dialog', pid: 4, active: false, owner: 'elsewhere' })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+s' }, { type: 'name' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, 'Save as')
  const into = process.platform === 'darwin' ? 'Untitled - Notepad' : 'Save as'
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'name', window: into, pid: 4 }])
})

test('a background plan with no app or wait step has no target and says so', async (t) => {
  const d = desktop([user(), { id: '5', title: 'Untitled - Paint', role: 'window', pid: 5, active: false }])
  await daemon(t, d.reply)
  const r = await main({ steps: [{ type: 'hi' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /has no target window/)
  assert.ok(!d.calls.some((c) => c.tool === 'desk_type'))
})

test('typing the driver could not confirm and that the window does not show escalates with the driver note', async (t) => {
  const d = desktop([user(), notepad()], ({ tool, args }) => tool === 'desk_type' ? JSON.stringify({ ok: true, chars: String(args.text).length, unverified: 'unverifiable: Sent 2 char(s) via PostMessage (not verified).' }) : undefined)
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.equal(r.ok, false)
  assert.equal(r.escalate?.step, 1)
  assert.match(String(r.escalate?.reason), /did not show .*PostMessage/)
})

test('a password the window does not show after typing escalates without the password', async (t) => {
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'focused', 'protected'], value: '' }
  const d = desktop([user(), notepad()], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad().title, states: ['showing'] }, field] })
    if (tool === 'desk_type') return JSON.stringify({ ok: true, chars: String(args.text).length, unverified: `unverifiable: Sent ${String(args.text).length} char(s), first h, via PostMessage` })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: '{{password}}', focus: { secure: true } }], params: { password: 'hunter2' } })
  assert.equal(r.ok, false)
  assert.equal(r.escalate?.reason, 'the password did not show in "Untitled - Notepad" after typing: unverifiable: the password was sent and the target did not confirm it; do not type it again')
  assert.ok(!JSON.stringify(r).includes('hunter2') && !JSON.stringify(r).includes('7 char') && !JSON.stringify(r).includes('first h'), JSON.stringify(r))
})

test('a window with no text field reads no unfocused label back, so its read-back is what it was', async (t) => {
  const labels = [1, 2, 3].map((k) => ({ ref: k, id: `1/${k + 1}`, parent: 0, depth: 1, role: 'text', name: `Label ${k}`, states: ['showing'] }))
  const d = desktop([user(), notepad()], ({ tool }) => tool === 'desk_snapshot' ? JSON.stringify({ window: notepad(), truncated: false, nodes: [root('window', notepad().title), ...labels] }) : undefined)
  await daemon(t, d.reply)
  await main({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.deepEqual(sent(d.calls, 'desk_read'), [])
})

test('a background type reads the unvalued fields of its window four at a time', async (t) => {
  const fields = Array.from({ length: 40 }, (_, k) => ({ ref: k + 1, id: `1/${k + 2}`, parent: 0, depth: 1, role: 'textbox', name: `Field ${k}`, states: ['showing'] }))
  const d = desktop([user(), notepad()], ({ tool }) => tool === 'desk_snapshot' ? JSON.stringify({ window: notepad(), truncated: false, nodes: [root('window', notepad().title), ...fields] }) : undefined)
  const held: (() => void)[] = []
  let base = 0; let answered = 0; let most = 0
  await daemon(t, async (c) => {
    if (c.tool !== 'desk_read') { base = calls.n; answered = 0; return d.reply(c) }
    await new Promise<void>((go) => {
      held.push(go)
      const open = calls.n - base - answered; most = Math.max(most, open)
      if (held.length === open) { answered += open; for (const release of held.splice(0)) release() }
    })
    return '{"value":""}'
  })
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(most, 4)
})

test('an element that is not in the tree is not looked for on the screen in the background', async (t) => {
  const d = desktop([user(), notepad()])
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { click: 'Bold' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /does not read the screen/)
  for (const tool of ['desk_shot', 'desk_click', 'model_ground']) assert.ok(!d.calls.some((c) => c.tool === tool), tool)
})

test('an app that brings itself to the front during a step fails that step with the reason', async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') { ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: true } }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'alt' }, { type: 'x' }] })
  assert.equal(r.ok, false)
  assert.equal(r.escalate?.step, 1)
  assert.match(String(r.escalate?.reason), /brought itself to the front/)
})

test('a macOS menu step in the background refuses before it acts', { skip: process.platform !== 'darwin' }, async (t) => {
  const d = desktop([user(), notepad()])
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { menu: 'Format > Font' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /menu bar belongs to the front app/)
  assert.ok(!d.calls.some((c) => c.tool === 'desk_press'))
})

test('input to a macOS sheet goes to the window it hangs on, which owns its pixels', { skip: process.platform !== 'darwin' }, async (t) => {
  const ws = [user(), { ...notepad(), title: 'Untitled' }]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') ws.push({ id: 's', title: 'save', role: 'dialog', pid: 4, active: true, owner: '1' })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Untitled' }, { key: 'cmd+s' }, { type: 'name' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'name', window: 'Untitled', pid: 4 }])
})

test('an untitled plain window the target owns, such as a console host, never becomes the target', async (t) => {
  const ws = [user(), notepad(), { id: 'h', title: '', role: 'window', pid: 4, active: false, owner: '1' }]
  const d = desktop(ws)
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'x' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'x', window: 'Untitled - Notepad', pid: 4 }])
})

test('a background plan reads its window by id and refuses input when another window of the app has the same title', async (t) => {
  const d = desktop([user(), notepad(), { ...notepad(), id: '2' }])
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /also titled "Untitled - Notepad"/)
  assert.ok(!d.calls.some((c) => c.tool === 'desk_type'))
  const snaps = sent(d.calls, 'desk_snapshot')
  assert.ok(snaps.length > 0 && snaps.every((a) => a.id === '1' && a.window === undefined), JSON.stringify(snaps))
})

test('an unnamed background close leaves another document\'s dialog open', async (t) => {
  const find: W = { id: 'f', title: 'Find', role: 'dialog', pid: 4, active: false, owner: '2' }
  const ws = [user(), notepad(), { id: '2', title: 'Notes - Notepad', role: 'window', pid: 4, active: false }, find]
  const dialog = JSON.stringify({ window: find, nodes: [{ ref: 0, id: 'f0', parent: -1, depth: 0, role: 'dialog', name: 'Find', states: [] }], truncated: false })
  const d = desktop(ws, ({ tool, args }) => tool === 'desk_snapshot' && (args.id === 'f' || args.window === 'Find') ? dialog : undefined)
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Untitled - Notepad' }, { close: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  for (const tool of ['desk_key', 'desk_close']) assert.ok(!d.calls.some((c) => c.tool === tool), tool)
})

test('a press the driver could not confirm and no later step reads back fails the plan with its note', async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: 'Untitled - Notepad', states: [] },
    { ref: 1, id: 'b', parent: 0, depth: 1, role: 'button', name: 'Bold', states: ['showing'], bounds: [10, 10, 20, 20] },
  ]
  const d = desktop([user(), notepad()], ({ tool }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes, truncated: false })
    if (tool === 'desk_press') return JSON.stringify({ ok: true, how: 'click', unverified: 'unverifiable: posted a click' })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { click: 'Bold' }] })
  assert.equal(r.ok, false)
  assert.equal(r.escalate?.step, 1)
  assert.match(String(r.escalate?.reason), /not confirmed.*posted a click/)
  assert.deepEqual(r.unverified, [{ step: 1, note: 'unverifiable: posted a click' }])
})

test('a later step confirms an unconfirmed press only when it reads something the press could change', async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: 'Untitled - Notepad', states: [] },
    { ref: 1, id: 'b', parent: 0, depth: 1, role: 'button', name: 'Bold', states: ['showing'], bounds: [10, 10, 20, 20] },
  ]
  const d = desktop([user(), notepad()], ({ tool }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes, truncated: false })
    if (tool === 'desk_press') return JSON.stringify({ ok: true, how: 'click', unverified: 'unverifiable: posted a click' })
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.9 } }, usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }, served: { model: 'm', build: null, id: null } })
    return undefined
  })
  await daemon(t, d.reply)
  for (const [check, ok] of [[{ wait: 'Notepad' }, false], [{ verify: 'Bold' }, false], [{ verify: 'Bold', enabled: true }, true], [{ expect: 'the text is bold' }, true]] as const) {
    const r = await main({ steps: [{ app: 'Notepad' }, { click: 'Bold' }, check] })
    assert.equal(r.ok, ok, `${JSON.stringify(check)} ${JSON.stringify(r)}`)
  }
})

test('typing the driver could not confirm passes only when a field holds the text more often than before it', async (t) => {
  for (const [after, ok] of [['hi there', false], ['hihi there', true]] as const) {
    let typed = false
    const doc = () => ({ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: typed ? after : 'hi there' })
    const d = desktop([user(), notepad()], ({ tool }) => {
      if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [doc()], truncated: false })
      if (tool === 'desk_type') { typed = true; return JSON.stringify({ ok: true, chars: 2, unverified: 'unverifiable: posted' }) }
      return undefined
    })
    await daemon(t, d.reply)
    const r = await main({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
    assert.equal(r.ok, ok, JSON.stringify(r))
    if (!ok) assert.match(String(r.escalate?.reason), /did not show/)
  }
})

test('the plan window coming to the front over the user\'s other window of the same app fails the step', async (t) => {
  const other = (): W => ({ id: 'o', title: 'Other - Notepad', role: 'window', pid: 4, active: true })
  const ws = [other(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') { ws[0] = { ...other(), active: false }; ws[1] = { ...notepad(), active: true } }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Untitled - Notepad' }, { key: 'alt' }, { type: 'x' }] })
  assert.equal(r.ok, false)
  assert.equal(r.escalate?.step, 1)
  assert.match(String(r.escalate?.reason), /brought itself to the front/)
})

test('two new macOS dialogs of the app with no owner make background input refuse instead of picking one', { skip: process.platform !== 'darwin' }, async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') ws.push({ id: 'a', title: 'Print', role: 'dialog', pid: 4, active: false }, { id: 'b', title: 'Find', role: 'dialog', pid: 4, active: false })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'cmd+p' }, { type: 'x' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /never guesses which one/)
  assert.ok(!d.calls.some((c) => c.tool === 'desk_type'))
})

test('a macOS background step, named for its window or not, stops with nothing sent while an untitled alert of the app is in front of it', { skip: process.platform !== 'darwin' }, async (t) => {
  const doc: W = { id: '198', title: 'r89aN3', role: 'window', pid: 4, active: false, app: 'R89Kit' }
  const alert: W = { id: '201', title: '', role: 'dialog', pid: 4, active: false, app: 'R89Kit' }
  for (const step of [{ click: 'Tool', window: 'r89aN3' }, { click: 'Tool' }]) {
    const ws = [user(), doc]
    const nodes = [{ ref: 0, id: '198/0', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }, { ref: 1, id: '198/1', parent: 0, depth: 1, role: 'button', name: 'Alert now', states: ['showing'] }, { ref: 2, id: '198/2', parent: 0, depth: 1, role: 'button', name: 'Tool', states: ['showing'] }]
    const d = desktop(ws, ({ tool, args }) => {
      if (tool === 'desk_snapshot') return JSON.stringify(args.id === alert.id ? { window: alert, nodes: [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }], truncated: false } : { window: doc, nodes, truncated: false })
      if (tool === 'desk_press') { if (args.id === '198/1') ws.push(alert); return '{"ok":true,"how":"click"}' }
      return undefined
    })
    await daemon(t, d.reply)
    const r = await main({ steps: [{ app: 'R89Kit' }, { click: 'Alert now' }, step] })
    assert.equal(r.escalate?.step, 2, JSON.stringify(r))
    assert.equal(r.escalate?.reason, TEXT)
    assert.deepEqual(sent(d.calls, 'desk_press'), [{ id: '198/1' }])
  }
})

test('a macOS background alert of the app open before the plan stops every input step, named or not, with nothing sent, an unnamed close closes it, and an unnamed close among two such dialogs stops', { skip: process.platform !== 'darwin' }, async (t) => {
  for (const [step, want, variant] of [[{ click: 'Tool', window: 'r89aN3' }, TEXT, ''], [{ click: 'Tool' }, TEXT, ''], [{ click: 'OK' }, TEXT, ''], [{ click: 'Delete' }, TEXT, 'menu'], [{ click: 'Delete', fp: { role: 'button', name: 'Tool', parent: 'r89aN3', nth: 0 } }, TEXT, ''], [{ click: 'Delete' }, TEXT, 'hidden'], [{ click: 'Delete' }, TEXT, 'unbounded'], [{ click: 'Delete' }, TEXT, 'static'], [{ check: 'Wrap', on: true }, TEXT, ''], [{ set: 'Name', value: 'x' }, TEXT, ''], [{ menu: 'Edit > Delete' }, TEXT, 'menu'], [{ type: 'x' }, TEXT, ''], [{ key: 'return' }, TEXT, ''], [{ close: true, window: 'r89aN3' }, TEXT, ''], [{ close: true }, '', ''], [{ close: true }, SEVERAL, 'two dialogs'], [{ type: 'x' }, TEXT, 'front'], [{ key: 'return' }, TEXT, 'front'], [{ click: 'OK' }, TEXT, 'front'], [{ close: true }, '', 'front'], [{ close: true }, SEVERAL, 'two front'], [{ close: true }, SEVERAL, 'two second front'], [{ click: 'Tool' }, TEXT, 'wait'], [{ key: 'return' }, TEXT, 'wait'], [{ type: 'x' }, TEXT, 'wait']] as const) {
    rmSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, { force: true })
    const doc: W = { id: '198', title: 'r89aN3', role: 'window', pid: 4, active: false, app: 'R89Kit' }
    const alert: W = { id: '201', title: '', role: 'dialog', pid: 4, active: variant === 'front' || variant === 'two front' || variant === 'wait', app: 'R89Kit' }
    const second: W = { id: '202', title: '', role: 'dialog', pid: 4, active: variant === 'two second front', app: 'R89Kit' }
    const at = variant === 'unbounded' ? { bounds: [10, 10, 40, 20] } : {}
    const nodes = [{ ref: 0, id: '198/0', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }, { ref: 1, id: '198/2', parent: 0, depth: 1, role: 'button', name: 'Tool', states: ['showing'], ...at }, { ref: 2, id: '198/3', parent: 0, depth: 1, role: variant === 'static' ? 'text' : 'button', name: variant === 'menu' ? 'Wrap' : 'Delete', states: variant === 'hidden' ? [] : ['showing'] }, { ref: 3, id: '198/4', parent: 0, depth: 1, role: 'checkbox', name: 'Wrap', states: ['showing'] }, { ref: 4, id: '198/5', parent: 0, depth: 1, role: 'textbox', name: 'Name', states: ['showing'] }, { ref: 5, id: '198/6', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] }, { ref: 6, id: '198/7', parent: 5, depth: 2, role: 'menu', name: 'Edit', states: ['showing'] }, { ref: 7, id: '198/8', parent: 6, depth: 3, role: 'menuitem', name: 'Delete', states: [] }]
    const own = [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }, { ref: 1, id: '201/1', parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'] }, { ref: 2, id: '201/2', parent: 0, depth: 1, role: 'button', name: 'Cancel', states: ['showing'] }, { ref: 3, id: '201/3', parent: 0, depth: 1, role: 'button', name: 'Delete', states: ['showing'] }]
    const ws = [{ ...user(), active: !alert.active && !second.active }, doc, alert, ...(variant.startsWith('two') ? [second] : [])].sort((a, b) => +b.active - +a.active)
    const d = desktop(ws, ({ tool, args }) => {
      if (tool === 'desk_snapshot') return JSON.stringify(args.id === doc.id ? { window: doc, nodes, truncated: false } : { window: alert, nodes: own, truncated: false })
      if (tool === 'desk_key' && args.keys === 'Escape') ws.splice(ws.indexOf(alert), 1)
      return undefined
    })
    await daemon(t, d.reply)
    const r = await main({ steps: [variant === 'wait' ? { wait: 'r89aN3' } : { app: 'R89Kit' }, step] })
    const input = d.calls.filter((c) => ['desk_press', 'desk_type', 'desk_key', 'desk_click', 'desk_close'].includes(c.tool)).map((c) => c.tool)
    if (!want) { assert.equal(r.ok, true, JSON.stringify(r)); assert.deepEqual(input, ['desk_key']); continue }
    assert.equal(r.escalate?.step, 1, `${JSON.stringify(step)} ${JSON.stringify(r)}`)
    assert.equal(r.escalate?.reason, want, JSON.stringify(step))
    assert.deepEqual(input, [], JSON.stringify(step))
  }
})

test('an untitled macOS popup of the app, such as AutoFill, is never taken for the plan\'s window', { skip: process.platform !== 'darwin' }, async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') ws.push({ id: 'p', title: '', role: 'generic', pid: 4, active: false })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'tab' }, { type: 'x' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type').map((a) => a.window), [notepad().title])
})

test('a background password reads back as its mask characters and counts as typed', async (t) => {
  for (const type of ['{{password}}', '{{password}}\n']) await masks(t, type)
})

async function masks(t: TestContext, type: string) {
  let value = ''
  const field = { ref: 1, id: 'p', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'focused', 'protected'] }
  const d = desktop([user(), notepad()], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [{ ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: notepad().title, states: [] }, { ...field, value }], truncated: false })
    if (tool === 'desk_type') { value += '•'.repeat([...String(args.text).replace(/\n/g, '')].length); return JSON.stringify({ ok: true, chars: 7, unverified: 'unverifiable: Posted via CGEvent.' }) }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type, focus: { secure: true } }], params: { password: 'hunter2' } })
  assert.equal(r.ok, true, `${JSON.stringify(type)} ${JSON.stringify(r)}`)
  assert.ok(!JSON.stringify(r).includes('hunter2'), JSON.stringify(r))
}

test('a plain background type into a field its own read shows as a focused password field is sealed before it goes out and makes the desk calls of a secret step', async (t) => {
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  const partial = ((await import(`../lib/gui_plan.mts?${run}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_RUN
  const tools: string[][] = []
  for (const step of [{ type: 'hunter13', secret: true }, { type: 'hunter13' }, { type: 'hunter13', secret: true }]) {
    let value = ''; const shown: string[] = []
    const d = desktop([user(), notepad()], ({ tool, args }) => {
      if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), truncated: false, nodes: [{ ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: notepad().title, states: [] }, { ref: 1, id: 'p', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'focused', 'protected'], value }] })
      if (tool === 'desk_type') { shown.push(readFileSync(`${run}/result.json`, 'utf8')); value += '•'.repeat(String(args.text).length); return JSON.stringify({ ok: true, chars: 8, unverified: 'unverifiable: Posted via CGEvent.' }) }
      return undefined
    })
    await daemon(t, d.reply)
    const r = await partial({ steps: [{ app: 'Notepad' }, step] })
    assert.equal(r.ok, true, JSON.stringify(r))
    const disk = ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
    for (const text of [JSON.stringify(r), disk, ...shown]) assert.ok(!text.includes('hunter13'), `${JSON.stringify(step)} ${text}`)
    tools.push(d.calls.map((c) => c.tool))
  }
  assert.deepEqual(tools[1], tools[2])
})

test('a plain background type right after a press, a double or right click, or a set on a password field is sealed though the field reads as not focused, as UIA reads a window behind', async (t) => {
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  const partial = ((await import(`../lib/gui_plan.mts?${run}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_RUN
  const field = { ref: 1, id: 'p', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'protected'], bounds: [10, 10, 100, 20] }
  const win = { ...notepad(), bounds: [0, 0, 800, 600] }
  for (const step of [{ click: 'Password' }, { click: 'Password', count: 2 }, { click: 'Password', button: 'right' }, ...(process.platform === 'win32' ? [] : [{ set: 'Password', value: 'pin' }])]) {
    const shown: string[] = []
    const d = desktop([user(), win], ({ tool, args }) => {
      if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: win.title, states: [] }, field] })
      if (tool === 'desk_read') return JSON.stringify(field)
      if (tool === 'desk_press') return JSON.stringify({ ok: true, how: 'click', x: 60, y: 20 })
      if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(800, 600).toString('base64') }
      if (tool === 'desk_type') { shown.push(readFileSync(`${run}/result.json`, 'utf8')); return JSON.stringify({ ok: true, chars: String(args.text).length, unverified: `unverifiable: Sent ${String(args.text).length} char(s) via PostMessage (not verified).` }) }
      return undefined
    })
    await daemon(t, d.reply)
    const r = await partial({ steps: [{ app: 'Notepad' }, step, { type: 'hunter2' }] })
    assert.ok(d.calls.some((c) => c.tool === 'desk_type' && c.args.text === 'hunter2'), `${JSON.stringify(step)} ${JSON.stringify(r)}`)
    const disk = ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
    for (const text of [JSON.stringify(r), disk, ...shown, readFileSync(`${run}/result.json`, 'utf8')]) assert.ok(!text.includes('hunter2'), `${JSON.stringify(step)} ${text}`)
  }
})

test('a pressed password field stays the field a background type goes to across a step that sends nothing, and a press on another element or a key drops it', async (t) => {
  const field = { ref: 1, id: 'p', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'protected'] }
  const button = { ref: 2, id: 's', parent: 0, depth: 1, role: 'button', name: 'Sign in', states: ['showing'] }
  for (const [between, sealed] of [[{ verify: 'Sign in' }, true], [{ click: 'Sign in' }, false], [{ key: 'tab' }, false]] as const) {
    const d = desktop([user(), notepad()], ({ tool, args }) => {
      if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), truncated: false, nodes: [{ ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: notepad().title, states: [] }, field, button] })
      if (tool === 'desk_read') return JSON.stringify(args.id === 's' ? button : field)
      if (tool === 'desk_press') return JSON.stringify({ ok: true, how: 'click', x: 60, y: 20 })
      if (tool === 'desk_type') return JSON.stringify({ ok: true, chars: String(args.text).length, unverified: `unverifiable: Sent ${String(args.text).length} char(s) via PostMessage (not verified).` })
      return undefined
    })
    await daemon(t, d.reply)
    const r = await main({ steps: [{ app: 'Notepad' }, { click: 'Password' }, between, { type: 'hunter2' }] })
    assert.ok(d.calls.some((c) => c.tool === 'desk_type' && c.args.text === 'hunter2'), `${JSON.stringify(between)} ${JSON.stringify(r)}`)
    const disk = ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
    assert.equal([JSON.stringify(r), disk].some((x) => x.includes('hunter2')), !sealed, `${JSON.stringify(between)} ${JSON.stringify(r)}`)
  }
})

test('a macOS background plan with no app step after another app\'s plan left its chain at that app\'s untitled alert works in that alert by the chain, never in the alert of the other app, and with no chain has no window at all', { skip: process.platform !== 'darwin' }, async (t) => {
  const other: W = { id: '301', title: '', role: 'dialog', pid: 3000, active: false, app: 'Other' }
  const alert: W = { id: '201', title: '', role: 'dialog', pid: 4, active: false, app: 'R89Kit' }
  const d = desktop([user(), other, alert], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: args.id === alert.id ? alert : other, nodes: [{ ref: 0, id: `${String(args.id)}/0`, parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }, { ref: 1, id: `${String(args.id)}/1`, parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'] }], truncated: false })
    return undefined
  })
  await daemon(t, d.reply)
  rmSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, { force: true })
  assert.equal((await main({ steps: [{ app: 'Other' }] })).ok, true)
  const r = await main({ steps: [{ click: 'OK' }] })
  assert.equal(r.escalate?.reason, TEXT.replace('"R89Kit" is in front of "r89aN3" and', '"Other"'), JSON.stringify(r))
  const chain = JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as Record<string, { pid: number; steps: unknown[] }>
  assert.deepEqual([chain['']?.pid, chain['']?.steps], [3000, [{ app: 'Other' }]])
  rmSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, { force: true })
  const fresh = await main({ steps: [{ click: 'OK' }] })
  assert.match(String(fresh.escalate?.reason), /^background gui_plan has no target window/, JSON.stringify(fresh))
  assert.ok(!d.calls.some((c) => ['desk_press', 'desk_click', 'desk_key', 'desk_type'].includes(c.tool)), JSON.stringify(d.calls.map((c) => c.tool)))
})

test('a macOS background call that continues the chain of an app step that failed at its first step sends nothing to an untitled dialog of the app', { skip: process.platform !== 'darwin' }, async (t) => {
  const alert: W = { id: '201', title: '', role: 'dialog', pid: 4, active: true, app: 'R89Kit' }
  const doc: W = { id: '198', title: 'r89aN3', role: 'window', pid: 4, active: false, app: 'R89Kit' }
  let stale = true
  const d = desktop([alert, doc], ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...alert, ...(stale ? { stale: true } : {}) }, { ...doc, ...(stale ? { stale: true } : {}) }, { ...user(), active: false }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: alert, nodes: [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }], truncated: false })
    return undefined
  })
  await daemon(t, d.reply)
  const failed = await main({ steps: [{ app: 'R89Kit' }] })
  assert.equal(failed.escalate?.step, 0, JSON.stringify(failed))
  stale = false
  const next = await main({ steps: [{ key: 'return' }] })
  assert.equal(next.ok, false, JSON.stringify(next))
  assert.ok(!d.calls.some((c) => c.tool === 'desk_key' || c.tool === 'desk_type'), JSON.stringify(next))
})

test('when the plan window is gone a window of the app with the same title never takes its place', async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') ws[1] = { ...notepad(), id: '2' }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+w' }, { type: 'x' }] })
  assert.equal(r.ok, false)
  assert.equal(r.escalate?.step, 2)
  assert.match(String(r.escalate?.reason), /is gone/)
  assert.ok(!d.calls.some((c) => c.tool === 'desk_type'))
})

test('a background click with a button, count or modifiers goes into the plan window in its image pixels, not to the covering window', async (t) => {
  const report = { id: '1', title: 'Report', role: 'window', pid: 4, active: false, bounds: [100, 50, 600, 400] }
  const file = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'report.txt', states: ['showing'], bounds: [300, 250, 40, 20] }
  const d = desktop([user(), report], ({ tool }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: report, truncated: false, nodes: [{ ref: 0, id: '1', parent: -1, depth: 0, role: 'window', name: 'Report', states: ['showing'] }, file] })
    if (tool === 'desk_read') return JSON.stringify(file)
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(1200, 800).toString('base64') }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Report' }, { click: 'report.txt', count: 2 }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_click'), [{ x: 440, y: 420, count: 2, window: 'Report', pid: 4 }])
  assert.deepEqual(sent(d.calls, 'desk_shot'), [{ window: 'Report', pid: 4 }])
  assert.equal(d.calls.some((c) => c.tool === 'desk_at'), false)
  const note = 'unverifiable: Posted click (not verified).'
  const u = desktop([user(), report], ({ tool }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: report, truncated: false, nodes: [{ ref: 0, id: '1', parent: -1, depth: 0, role: 'window', name: 'Report', states: ['showing'] }, file] })
    if (tool === 'desk_read') return JSON.stringify(file)
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(1200, 800).toString('base64') }
    if (tool === 'desk_click') return JSON.stringify({ ok: true, unverified: note })
    return undefined
  })
  await daemon(t, u.reply)
  const heard = await main({ steps: [{ app: 'Report' }, { click: 'report.txt', count: 2 }] })
  assert.deepEqual(heard.unverified, [{ step: 1, note }], JSON.stringify(heard))
})

test('a background check whose state cannot be read after its press says the control was pressed', async (t) => {
  let pressed = false
  const box = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'checkbox', name: 'Wrap lines', states: ['showing'] }
  const d = desktop([user(), notepad()], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad().title, states: ['showing'] }, box] })
    if (tool === 'desk_press' && args.id === '1/2') pressed = true
    if (tool === 'desk_read' && pressed) throw new Error('unknown element id 1/2')
    if (tool === 'desk_read') return JSON.stringify(box)
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { check: 'Wrap lines', on: true }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /^"Wrap lines" was pressed, and its state after the press needs a look at the screen: /)
  assert.deepEqual(sent(d.calls, 'desk_press').map((a) => a.id), ['1/2'])
})

test('a background click with a count takes the image scale from the window of its own app when another app has a window of that title', async (t) => {
  const finder = { id: '1', title: 'Downloads', role: 'window', pid: 4, active: false, app: 'Finder', bounds: [100, 50, 600, 400] }
  const browser = { id: '2', title: 'Downloads', role: 'window', pid: 8, active: false, app: 'Browser', bounds: [0, 0, 300, 200] }
  const file = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'report.txt', states: ['showing'], bounds: [300, 250, 40, 20] }
  const d = desktop([user(), browser, finder], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: finder, truncated: false, nodes: [{ ref: 0, id: '1', parent: -1, depth: 0, role: 'window', name: 'Downloads', states: ['showing'] }, file] })
    if (tool === 'desk_read') return JSON.stringify(file)
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: (args.pid === 4 ? gray(1200, 800) : gray(300, 200)).toString('base64') }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Finder' }, { click: 'report.txt', count: 2 }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_click'), [{ x: 440, y: 420, count: 2, window: 'Downloads', pid: 4 }])
})

test('a fresh background run saves the chain the last plan left at its window and locates in the front window', async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: 'Untitled - Notepad', states: [] },
    { ref: 1, id: 'b', parent: 0, depth: 1, role: 'button', name: 'Bold', states: ['showing'], bounds: [10, 10, 20, 20] },
  ]
  const d = desktop([user(), notepad()], ({ tool }) => tool === 'desk_snapshot' ? JSON.stringify({ window: notepad(), nodes, truncated: false }) : undefined)
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+b' }, { click: 'Italic' }] })
  assert.equal(r.escalate?.step, 2, JSON.stringify(r))
  const query = 'fresh'
  const fresh = ((await import(`../lib/gui_plan.mts?${query}`)) as typeof import('../lib/gui_plan.mts')).main
  const saved = await fresh({ save: 'bold' })
  assert.equal(saved.ok, true, JSON.stringify(saved))
  const found = await fresh({ locate: { target: 'Bold' } })
  assert.equal(found.found, true, JSON.stringify(found))
})

test('a Windows menu toggle that needs pixels refuses in the background and closes the menu it opened', { skip: process.platform !== 'win32' }, async (t) => {
  const state = { open: false }
  const nodes = () => [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad().title, states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: 'Application', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'View', states: ['showing'], bounds: [100, 0, 40, 20] },
    ...(state.open ? [
      { ref: 3, id: 'p/1', parent: 0, depth: 1, role: 'menu', name: 'View', states: ['showing'], bounds: [100, 20, 140, 44] },
      { ref: 4, id: 'p/2', parent: 3, depth: 2, role: 'menuitem', name: 'Status Bar', states: ['showing'], bounds: [100, 20, 140, 22] },
    ] : []),
  ]
  const d = desktop([user(), notepad()], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: nodes(), truncated: false })
    if (tool === 'desk_press' && args.id === '1/3') state.open = true
    if (tool === 'desk_key' && args.keys === 'Escape') state.open = false
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { menu: 'View > Status Bar', on: false }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /needs a look at the screen/)
  assert.deepEqual(sent(d.calls, 'desk_press').map((a) => a.id), ['1/3'])
  assert.equal(state.open, false)
})

test('a Windows menu toggle read as already on passes with no note of the unconfirmed click that opened its menu', { skip: process.platform !== 'win32' }, async (t) => {
  const state = { open: false }
  const item = { ref: 4, id: 'p/2', parent: 3, depth: 2, role: 'menuitemcheckbox', name: 'Status bar', states: ['showing', 'checked'], bounds: [100, 20, 140, 22] }
  const nodes = () => [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad().title, states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: 'Application', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'View', states: ['showing'], bounds: [100, 0, 40, 20] },
    ...(state.open ? [{ ref: 3, id: 'p/1', parent: 0, depth: 1, role: 'menu', name: 'View', states: ['showing'], bounds: [100, 20, 140, 44] }, item] : []),
  ]
  const d = desktop([user(), notepad()], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: nodes(), truncated: false })
    if (tool === 'desk_read') return JSON.stringify(item)
    if (tool === 'desk_press' && args.id === '1/3') { state.open = true; return JSON.stringify({ ok: true, how: 'click', unverified: 'unverifiable: Sent, effect not verified: Performed UIA Invoke at (129,49) for pid 4.' }) }
    if (tool === 'desk_key' && args.keys === 'Escape') state.open = false
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { menu: 'View > Status bar', on: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.unverified, undefined, JSON.stringify(r))
  assert.deepEqual([r.trace?.[1]?.was, r.trace?.[1]?.noop], ['on', true], JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_press').map((a) => a.id), ['1/3'])
  assert.equal(state.open, false)
})

function saveAs(start: string, stuck = false) {
  const ws: W[] = [user(), notepad(), { id: 's', title: 'Save as', role: 'dialog', pid: 4, active: false, owner: '1' }]
  const field = { value: start, saved: '' }
  const shown = () => field.value ? { value: field.value.slice(0, 200) } : {}
  const d = desktop(ws, ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: ws[2], truncated: false, nodes: [root('dialog', 'Save as'), { ref: 1, id: 's/2', parent: 0, depth: 1, role: 'combobox', name: 'File name:', states: ['showing'], ...shown() }, { ref: 2, id: 's/3', parent: 1, depth: 2, role: 'textbox', name: 'File name:', states: ['showing', 'focused'], ...shown() }, { ref: 3, id: 's/4', parent: 0, depth: 1, role: 'button', name: 'Save', states: ['showing'] }, { ref: 4, id: 's/5', parent: 0, depth: 1, role: 'button', name: 'Cancel', states: ['showing'] }] })
    if (tool === 'desk_type') { field.value += String(args.text); return JSON.stringify({ ok: true, chars: String(args.text).length, unverified: `unverifiable: Sent ${String(args.text).length} char(s) via PostMessage (not verified).` }) }
    if (tool === 'desk_key' && args.keys === 'backspace' && !stuck) field.value = field.value.slice(0, Math.max(0, field.value.length - Number(args.repeat ?? 1)))
    if (tool === 'desk_key' && args.keys === 'Return' && ws.length > 2) { field.saved = field.value; ws.pop() }
    return undefined
  })
  return { ...d, field }
}

test('a Windows background name typed into a file chooser replaces the name in its field with End and one Backspace per character, never Home and Shift+End', { skip: process.platform !== 'win32' }, async (t) => {
  const d = saveAs('n93w7.txt')
  await daemon(t, d.reply)
  const r = await main({ steps: [{ wait: 'Save as' }, { type: 'r93.txt\n' }] })
  assert.deepEqual(keysBefore(d.calls, 'desk_type'), [{ keys: 'end', window: 'Save as', pid: 4 }, { keys: 'backspace', window: 'Save as', pid: 4, repeat: 9 }])
  assert.equal(d.field.saved, 'r93.txt')
  assert.equal(r.ok, true, JSON.stringify(r))
})

test('a Windows background file chooser whose field reads no name, or a name the desk cut, gets 260 Backspaces', { skip: process.platform !== 'win32' }, async (t) => {
  for (const start of ['', 'x'.repeat(230)]) {
    const d = saveAs(start)
    await daemon(t, d.reply)
    const r = await main({ steps: [{ wait: 'Save as' }, { type: 'r93.txt\n' }] })
    assert.deepEqual(keysBefore(d.calls, 'desk_type').map((a) => [a.keys, a.repeat]), [['end', undefined], ['backspace', 256], ['backspace', 4]])
    assert.equal(d.field.saved, 'r93.txt')
    assert.equal(r.ok, true, JSON.stringify(r))
  }
})

test('a Windows background name that leaves other text in the file chooser field stops before its Return and names the field', { skip: process.platform !== 'win32' }, async (t) => {
  const d = saveAs('n93w7.txt', true)
  await daemon(t, d.reply)
  const r = await main({ steps: [{ wait: 'Save as' }, { type: 'r93.txt\n' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, '"r93.txt" did not replace the text of field "File name:" in "Save as", which reads "n93w7.txtr93.txt", so it was not submitted')
  assert.ok(!sent(d.calls, 'desk_key').some((a) => a.keys === 'Return'), JSON.stringify(sent(d.calls, 'desk_key')))
  assert.equal(d.field.saved, '')
})

test('a recorded after names a new window of the app, which the background plan then takes as its target', async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') ws.push({ id: '2', title: 'Untitled 2 - Notepad', role: 'window', pid: 4, active: false })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+n', after: 'Untitled 2 - Notepad' }, { type: 'x' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'x', window: 'Untitled 2 - Notepad', pid: 4 }])
})

test('a recorded after takes the one window of the app with that exact title over a longer title listed first, and no window when two titles only contain it', async (t) => {
  for (const [first, second, want] of [['Untitled 20', 'Untitled 2', 'Untitled 2'], ['Report 1 draft', 'Report 1 final', null]] as const) {
    const ws = [user(), notepad()]
    const d = desktop(ws, ({ tool }) => {
      if (tool === 'desk_key') ws.push({ id: '3', title: first, role: 'window', pid: 4, active: false }, { id: '2', title: second, role: 'window', pid: 4, active: false })
      return undefined
    })
    await daemon(t, d.reply)
    const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+n', after: want ?? 'Report 1', timeout_ms: 300 }, { type: 'x' }] })
    assert.equal(r.ok, want !== null, JSON.stringify(r))
    assert.deepEqual(sent(d.calls, 'desk_type'), want ? [{ text: 'x', window: want, pid: 4 }] : [])
  }
})

test('a recorded after never takes a dialog of the app that the plan window does not own', async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') ws.push({ id: 'd', title: 'Untitled 2', role: 'dialog', pid: 4, active: false, owner: 'elsewhere' })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+n', after: 'Untitled 2', timeout_ms: 300 }, { type: 'x' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /recording saw "Untitled 2" in front/)
  assert.deepEqual(sent(d.calls, 'desk_type'), [])
})

test('a recorded after waits for the window the step opens and never takes an older window whose title contains it', async (t) => {
  const ws = [user(), notepad(), { id: '3', title: 'Untitled 20', role: 'window', pid: 4, active: false }]
  let lists = -1
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_key') lists = 0
    if (tool === 'desk_windows' && lists >= 0 && ++lists === 3) ws.push({ id: '2', title: 'Untitled 2', role: 'window', pid: 4, active: false })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+n', after: 'Untitled 2' }, { type: 'x' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'x', window: 'Untitled 2', pid: 4 }])
})

test('a background Return that closes its dialog is confirmed by the dialog leaving', async (t) => {
  const ask = { id: '2', title: 'Save changes?', role: 'window', pid: 4, active: false }
  const ws = [user(), notepad(), ask]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: ask, truncated: false, nodes: [{ ref: 0, id: '2', parent: -1, depth: 0, role: 'dialog', name: ask.title, states: ['showing'] }] })
    if (tool !== 'desk_key') return undefined
    ws.pop()
    return JSON.stringify({ ok: true, unverified: 'unverifiable: Posted Return (not verified).' })
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Save changes?' }, { key: 'Return' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.unverified, undefined)
})

test('a background line whose Return closes its dialog stays unconfirmed, and is not reported as text that did not show', async (t) => {
  const go = { id: '2', title: 'Go To Line', role: 'window', pid: 4, active: false }
  const ws = [user(), notepad(), go]
  const note = 'unverifiable: Sent via PostMessage (not verified).'
  const d = desktop(ws, ({ tool, args }) => {
    if (tool === 'desk_snapshot') { if (!ws.includes(go)) return { text: 'no window "Go To Line"', error: true }; return JSON.stringify({ window: go, truncated: false, nodes: [{ ref: 0, id: '2', parent: -1, depth: 0, role: 'dialog', name: go.title, states: ['showing'] }, { ref: 1, id: '2/1', parent: 0, depth: 1, role: 'textbox', name: 'Line number', states: ['showing', 'focused'], value: '' }] }) }
    if (tool !== 'desk_type') return undefined
    if (String(args.text).includes('\n')) ws.pop()
    return JSON.stringify({ ok: true, chars: String(args.text).length, unverified: note })
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Go To Line' }, { type: '10\n' }] })
  assert.match(String(r.escalate?.reason), /^step 1 \(type\) was sent but not confirmed/, JSON.stringify(r))
  assert.deepEqual(r.unverified, [{ step: 1, note }])
  assert.deepEqual(sent(d.calls, 'desk_type').map((a) => a.text), ['10', '\n'])
})

test('a recorded after that the window the step opened satisfies confirms the step the driver could not', async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool !== 'desk_key') return undefined
    ws.push({ id: '2', title: 'Untitled 2', role: 'window', pid: 4, active: false })
    return JSON.stringify({ ok: true, unverified: 'unverifiable: ctrl+n is the key equivalent of File > New' })
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+n', after: 'Untitled 2' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.unverified, undefined)
})

test('a recorded after that was already in front before the step leaves the step the driver could not confirm unverified', async (t) => {
  const d = desktop([user(), notepad()], ({ tool }) => tool === 'desk_key' ? JSON.stringify({ ok: true, unverified: 'unverifiable: posted' }) : undefined)
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+s', after: 'Untitled - Notepad' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /step 1 \(key\) was sent but not confirmed/)
})

test('a window that takes the plan window id under another process is not the plan window, and no input goes to it', async (t) => {
  const ws = [user(), notepad()]
  const d = desktop(ws, ({ tool }) => {
    if (tool === 'desk_type' && ws[1]?.pid === 4) { ws[1] = { ...notepad(), title: 'Other app', pid: 7 }; return JSON.stringify({ ok: true, chars: 1 }) }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'a' }, { type: 'b' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /plan's window "Untitled - Notepad" is gone/)
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'a', window: 'Untitled - Notepad', pid: 4 }])
})

test('an expect the tree does not meet fails a background plan without a look at the screen', async (t) => {
  const d = desktop([user(), notepad()], ({ tool }) => tool === 'model_decide' ? JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: null, id: null }, usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } }) : undefined)
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { expect: 'the text is bold' }] })
  assert.equal(r.ok, false)
  assert.equal(r.escalate?.step, 1)
  assert.match(String(r.escalate?.reason), /^expectation not met in the window's tree: the text is bold; background gui_plan does not read the screen/)
  for (const tool of ['desk_shot', 'model_ask', 'desk_focus']) assert.ok(!d.calls.some((c) => c.tool === tool), tool)
})

test('a background wait whose text several windows contain takes none of them', async (t) => {
  const one = { id: '2', title: 'Report 1 - Viewer', role: 'window', pid: 7, active: false }
  const two = { id: '3', title: 'Report 2 - Viewer', role: 'window', pid: 7, active: false }
  const d = desktop([user(), one, two])
  await daemon(t, d.reply)
  const r = await main({ steps: [{ wait: 'Viewer', timeout_ms: 400 }, { type: 'x' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.step, 0)
  assert.match(String(r.escalate?.reason), /2 windows match "Viewer"/)
  assert.ok(!d.calls.some((c) => c.tool === 'desk_type'))
})

test('a background wait takes the one window whose title is exactly the text over others that contain it', async (t) => {
  const one = { id: '2', title: 'Viewer', role: 'window', pid: 7, active: false }
  const two = { id: '3', title: 'Report 2 - Viewer', role: 'window', pid: 7, active: false }
  const d = desktop([user(), two, one])
  await daemon(t, d.reply)
  const r = await main({ steps: [{ wait: 'Viewer', timeout_ms: 400 }, { type: 'x' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'x', window: 'Viewer', pid: 7 }])
})

test('a plan stopped for a step sent but not confirmed or for a theft counts as done every step that ran, and its chain record ends before the step to check', async (t) => {
  for (const [steps, done] of [
    [[{ app: 'Notepad' }, { key: 'ctrl+a' }, { type: 'hello' }], 3],
    [[{ app: 'Notepad' }, { key: 'alt' }, { type: 'x' }], 2],
    [[{ app: 'Notepad' }, { key: 'alt' }], 2],
  ] as const) {
    const ws = [user(), notepad()]
    const d = desktop(ws, ({ tool, args }) => {
      if (tool !== 'desk_key') return undefined
      if (args.keys === 'alt') { ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: true } }
      return JSON.stringify({ ok: true, ...(args.keys === 'ctrl+a' ? { unverified: 'unverifiable: posted' } : {}) })
    })
    await daemon(t, d.reply)
    const r = await main({ steps: [...steps] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.equal(r.escalate?.step, 1, JSON.stringify(r))
    assert.equal(r.done, done, JSON.stringify(r))
    const chain = (JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as Record<string, { steps: unknown[] }>)['']
    assert.deepEqual(chain?.steps, [{ app: 'Notepad' }], JSON.stringify(chain))
  }
})

test('a background locate that presses its element carries the note of a press the driver could not confirm', async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: 'Calculator', states: [] },
    { ref: 1, id: 'b', parent: 0, depth: 1, role: 'button', name: 'Bold', states: ['showing'], bounds: [10, 10, 20, 20] },
  ]
  const d = desktop([user(), notepad()], ({ tool }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: user(), nodes, truncated: false })
    if (tool === 'desk_press') return JSON.stringify({ ok: true, how: 'click', unverified: 'unverifiable: posted a click' })
    return undefined
  })
  await daemon(t, d.reply)
  const query = 'locate'
  const fresh = ((await import(`../lib/gui_plan.mts?${query}`)) as typeof import('../lib/gui_plan.mts')).main
  const r = await fresh({ locate: { target: 'Bold', press: true } })
  assert.equal(r.found, true, JSON.stringify(r))
  assert.equal(r.via, 'click', JSON.stringify(r))
  assert.equal(r.unverified, 'unverifiable: posted a click', JSON.stringify(r))
})

test('a background app step whose launch raised the running app\'s window takes that window in front as the plan\'s own', async (t) => {
  const term = (): W => ({ id: 't', title: 'Windows PowerShell', role: 'window', pid: 12, active: false, app: 'WindowsTerminal' })
  const ws = [user(), term()]
  const d = desktop(ws, ({ tool }) => {
    if (tool !== 'desk_launch') return undefined
    ws[0] = { ...user(), active: false }; ws[1] = { ...term(), active: true }
    return JSON.stringify({ ok: true, launched: 'raised', running: true, title: 'Windows PowerShell', pid: 12 })
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Windows Terminal' }, { type: 'dir' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type'), [{ text: 'dir', window: 'Windows PowerShell', pid: 12 }])
})

test('a background terminal line into a window no read reaches waits for no settled read after its Return', async (t) => {
  const term: W = { id: '1', title: 'Terminal', role: 'window', pid: 4, active: false }
  const note = 'unverifiable: "Terminal" is on another Space'
  const d = desktop([user(), term], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return { text: '"Terminal" is on another Space', error: true }
    if (tool === 'desk_type') return JSON.stringify({ ok: true, chars: String(args.text).length, terminal: true, unverified: note })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Terminal' }, { type: 'a\nb' }] })
  assert.deepEqual(r.unverified, [{ step: 1, note }], JSON.stringify(r))
  assert.deepEqual(sent(d.calls, 'desk_type').map((a) => a.text), ['a', 'b'])
  assert.ok(sent(d.calls, 'desk_snapshot').length < 10, String(sent(d.calls, 'desk_snapshot').length))
})

const STALE = { text: 'window list is stale: "Terminal" did not answer in time, so its app is busy or hung; wait and retry, or close the app', error: true }

test('a background terminal line reads its window again after one stale read that follows its Return', async (t) => {
  const term: W = { id: '1', title: 'Terminal', role: 'window', pid: 4, active: false }
  let value = ''; let fail = false
  const d = desktop([user(), term], ({ tool, args }) => {
    if (tool === 'desk_snapshot') { if (fail) { fail = false; return STALE } return JSON.stringify({ window: term, truncated: false, nodes: [root('window', 'Terminal'), { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'terminal', name: '', states: ['showing'], value }] }) }
    if (tool === 'desk_type') { value += String(args.text); return JSON.stringify({ ok: true, chars: String(args.text).length, terminal: true, unverified: 'unverifiable: Typed (terminal emulator: pty/XTest key events)' }) }
    if (tool === 'desk_key' && args.keys === 'Return') { value += '\n'; fail = true }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Terminal' }, { type: 'a\nb' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(value, 'a\nb')
})

test('a background password line whose first read after typing is stale is still read for its echo before its Return', async (t) => {
  let value = ''; let fail = false
  const d = desktop([user(), notepad()], ({ tool, args }) => {
    if (tool === 'desk_snapshot') { if (fail) { fail = false; return STALE } return JSON.stringify({ window: notepad(), truncated: false, nodes: [root('window', notepad().title), { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused'], value }] }) }
    if (tool === 'desk_type') { value += String(args.text); fail = !String(args.text).includes('\n'); return JSON.stringify({ ok: true, chars: String(args.text).length, unverified: 'unverifiable: Posted via CGEvent.' }) }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: '{{pw}}\n', secret: true }], params: { pw: 'hunter2' } })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /^the password showed in "Untitled - Notepad", so it was not submitted/)
  assert.deepEqual(sent(d.calls, 'desk_type').map((a) => a.text), ['hunter2'])
})
