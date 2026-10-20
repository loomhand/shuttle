import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import type { Call } from './daemon.mts'

process.env.SHUTTLE_INPUT = 'background'
const { main } = await import('../lib/gui_plan.mts')

type W = { id: string; title: string; role: string; pid: number; active: boolean; owner?: string }

function desktop(ws: W[], answer: (c: Call) => string | undefined = () => undefined) {
  const calls: Call[] = []
  const reply = (c: Call) => {
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

const user = (): W => ({ id: 'c', title: 'Calculator', role: 'window', pid: 9, active: true })
const notepad = (): W => ({ id: '1', title: 'Untitled - Notepad', role: 'window', pid: 4, active: false })
const sent = (calls: Call[], tool: string) => calls.filter((c) => c.tool === tool).map((c) => c.args)

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

test('an unnamed background close shuts only a dialog the plan window owns and leaves another document\'s dialog open', async (t) => {
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
