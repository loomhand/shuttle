import assert from 'node:assert/strict'
import { test } from 'node:test'

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
    if (tool === 'desk_type') return JSON.stringify({ ok: true, chars: String(args.text).length, unverified: 'unverifiable: posted' })
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: '{{password}}', focus: { secure: true } }], params: { password: 'hunter2' } })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /^the password did not show in "Untitled - Notepad" after typing: unverifiable: posted$/)
  assert.ok(!JSON.stringify(r).includes('hunter2'), JSON.stringify(r))
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
  let value = ''
  const field = { ref: 1, id: 'p', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'focused', 'protected'] }
  const d = desktop([user(), notepad()], ({ tool, args }) => {
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [{ ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: notepad().title, states: [] }, { ...field, value }], truncated: false })
    if (tool === 'desk_type') { value += '•'.repeat([...String(args.text)].length); return JSON.stringify({ ok: true, chars: 7, unverified: 'unverifiable: Posted via CGEvent.' }) }
    return undefined
  })
  await daemon(t, d.reply)
  const r = await main({ steps: [{ app: 'Notepad' }, { type: '{{password}}', focus: { secure: true } }], params: { password: 'hunter2' } })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.ok(!JSON.stringify(r).includes('hunter2'), JSON.stringify(r))
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
