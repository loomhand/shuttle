import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('a step in a window whose app holds a modal dialog the screen does not show escalates with its title and types nothing', async (t) => {
  const wins = [
    { id: '1.13/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 833, active: true },
    { id: '1.13/339', title: 'Save As', role: 'dialog', pid: 833, active: false, modal: 'hidden', blocks: 'Untitled 1 - Mousepad' },
    { id: '1.18/40', title: 'Terminal - user@box: ~', role: 'window', pid: 1555, active: false },
  ]
  const calls: string[] = []
  const [doc, dialog, terminal] = wins
  assert.ok(doc && dialog && terminal)
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify(wins)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const blocked = await main({ steps: [{ type: 'hello' }] })
  assert.equal(blocked.ok, false, JSON.stringify(blocked))
  assert.equal(blocked.escalate?.reason, 'modal dialog "Save As" is open but hidden, and "Untitled 1 - Mousepad" takes no input until it closes: finish it, or close it with {close: true, window: "Save As"}')
  assert.ok(blocked.escalate?.windows?.includes('Save As (modal dialog, hidden)'), JSON.stringify(blocked.escalate))
  assert.ok(!calls.includes('desk_type'), calls.join(' '))
  doc.active = false
  dialog.active = true
  const itself = await main({ steps: [{ type: 'hello' }] })
  assert.equal(itself.escalate?.reason, blocked.escalate?.reason, JSON.stringify(itself))
  doc.active = true
  dialog.active = false
  const closing = await main({ steps: [{ close: true, window: 'Save As' }] })
  assert.ok(calls.includes('desk_key'), JSON.stringify(closing))
  doc.active = false
  terminal.active = true
  const other = await main({ steps: [{ type: 'hello' }] })
  assert.equal(other.ok, true, JSON.stringify(other))
  assert.ok(calls.includes('desk_type'), calls.join(' '))
})

test('a hidden modal dialog with no title names the ways out that close it on this system', async (t) => {
  const doc = { id: '1.13/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 833, active: true }
  const wins = [doc, { id: '1.13/339', title: '', role: 'dialog', pid: 833, active: false, modal: 'hidden', blocks: doc.title }]
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify(wins)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ type: 'hello' }] })
  const desk = 'press its button with the desk tool (snapshot with the dialog\'s id from desk windows, then press with the button\'s id)'
  const ways = process.platform === 'darwin' ? `${desk}, or close it with {close: true} when it has a Cancel button` : `close it with {close: true}, or ${desk}`
  assert.equal(r.escalate?.reason, `modal dialog "" is open but hidden, and "Untitled 1 - Mousepad" takes no input until it closes: finish it, or ${ways}`)
})

test('a close step fails when the dialog it closes stays open', async (t) => {
  const editor = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const find = { id: '2', title: 'Find', role: 'dialog', pid: process.pid, active: true }
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([editor, find])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: find, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Find', states: ['showing'] }] })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ close: true, window: 'Find' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /"Find" is still open/)
  assert.ok(calls.includes('desk_close'), calls.join(' '))
})

test('a close step closes the dialog it read by its id, and a dialog of the same title and process stays and does not fail it', async (t) => {
  const editor = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const finds = [{ id: '2', title: 'Find', role: 'dialog', pid: process.pid, active: true }, { id: '3', title: 'Find', role: 'dialog', pid: process.pid, active: false }]
  const closes: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([editor, ...finds])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: finds[0], truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Find', states: ['showing'] }] })
    if (tool === 'desk_close') { closes.push(args.id); finds.splice(finds.findIndex((f) => f.id === args.id), 1); editor.active = true }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ close: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(closes, ['2'])
  assert.deepEqual(finds.map((f) => f.id), ['3'])
})

test('a close step whose dialog no other window of the process shares a title with sends its close by title alone', async (t) => {
  const editor = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const find = { id: '2', title: 'Find', role: 'dialog', pid: process.pid, active: true }
  let open = true
  const sent: { tool: string; args: Record<string, unknown> }[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(open ? [editor, find] : [{ ...editor, active: true }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: find, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Find', states: ['showing'] }] })
    if (tool === 'desk_close') { sent.push({ tool, args }); open = false }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ close: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent, [{ tool: 'desk_close', args: { window: 'Find', pid: process.pid } }])
})

test('a close step that names its window sends the id of the window it picked when another window of the process has its title, and the title alone when none has', async (t) => {
  for (const twin of [true, false]) {
    const editor = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
    const finds = [{ id: '2', title: 'Find', role: 'dialog', pid: process.pid, active: true }, ...(twin ? [{ id: '3', title: 'Find', role: 'dialog', pid: process.pid, active: false }] : [])]
    const sent: Record<string, unknown>[] = []
    await daemon(t, ({ tool, args }) => {
      if (tool === 'desk_windows') return JSON.stringify([{ ...editor, active: !finds.some((f) => f.active) }, ...finds])
      if (tool === 'desk_snapshot') return JSON.stringify({ window: finds[0], truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Find', states: ['showing'] }] })
      if (tool === 'desk_close') {
        sent.push(args)
        if (twin && args.id === undefined) return { text: '2 windows match the title "Find", so nothing was sent: pass the window\'s id from desk windows', error: true }
        finds.splice(finds.findIndex((f) => f.id === (args.id ?? '2')), 1)
      }
      return '{"ok":true}'
    })
    const r = await main({ steps: [{ close: true, window: 'Find' }] })
    assert.equal(r.ok, true, `${twin} ${JSON.stringify(r)}`)
    assert.deepEqual(sent, [{ window: 'Find', pid: process.pid, ...(twin ? { id: '2' } : {}) }])
    assert.deepEqual(finds.map((f) => f.id), twin ? ['3'] : [])
  }
})

test('a close step with no window in front and a window listed as not answering sends nothing and is not ok', async (t) => {
  const panel = { id: '1.2/38', title: '', role: 'window', pid: 1206, active: false, app: 'xfce4-panel', shell: true }
  const editor = { id: '1.9/116', title: '~/a.txt - Mousepad', role: 'window', pid: 1339, active: false, app: 'mousepad', stale: true }
  const term = { id: '1.10/39', title: 'term', role: 'window', pid: 1353, active: false, app: 'xfce4-terminal' }
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([panel, editor, term])
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ close: true }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, 'no window is in front, and "~/a.txt - Mousepad" is listed as not answering accessibility, as an app hung behind its own dialog leaves it: nothing was sent; wait for it, or close the app')
  assert.ok(!calls.includes('desk_key') && !calls.includes('desk_close'), calls.join(' '))
})

async function unconfirmed(t: TestContext, note: string) {
  const editor = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const find = { id: '2', title: 'Find', role: 'dialog', pid: process.pid, active: true }
  let open = true
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify(open ? [editor, find] : [{ ...editor, active: true }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: find, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Find', states: ['showing'] }] })
    if (tool === 'desk_close') { open = false; return JSON.stringify({ ok: true, title: 'Find', unverified: note }) }
    return '{"ok":true}'
  })
  return calls
}

test('an Android close step whose close answers unverified stops with that note and sends nothing after it', async (t) => {
  const note = 'the window was sent the back key and left the window list, but "com.android.settings" came to the front, not another app, so it may still be open: read the window list before closing again'
  const mode = 'android-close'
  process.env.SHUTTLE_DESK = 'android'
  let device: typeof main
  try { device = ((await import(`../lib/gui_plan.mts?${mode}`)) as { main: typeof main }).main } finally { delete process.env.SHUTTLE_DESK }
  const calls = await unconfirmed(t, note)
  const r = await device({ steps: [{ close: true }, { key: 'Return' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, `"Find" may still be open after its close: ${note}`)
  assert.deepEqual(calls.filter((c) => c === 'desk_key' || c === 'desk_close'), ['desk_close'])
})

test('a desktop close step whose close answers unverified passes once the window leaves the list', async (t) => {
  const calls = await unconfirmed(t, 'the window did not answer the close within 500 ms, so it may still be closing, be asking to save, or not respond: read the window list before closing again')
  const r = await main({ steps: [{ close: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.ok(calls.includes('desk_close'), calls.join(' '))
})

async function deviceClose(t: TestContext, stays: boolean) {
  const mode = `android-close-id-${stays}`
  process.env.SHUTTLE_DESK = 'android'
  let device: typeof main
  try { device = ((await import(`../lib/gui_plan.mts?${mode}`)) as { main: typeof main }).main } finally { delete process.env.SHUTTLE_DESK }
  const editor = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const find = { id: '2', title: 'Find', role: 'dialog', pid: process.pid, active: true }
  let open = true
  const sent: Record<string, unknown>[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(open ? [editor, find] : [{ ...editor, active: true }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: find, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Find', states: ['showing'] }] })
    if (tool === 'desk_close') { sent.push(args); open = stays }
    return '{"ok":true}'
  })
  return { r: await device({ steps: [{ close: true }] }), sent }
}

test('an Android close step names the window it picked by id', async (t) => {
  const { r, sent } = await deviceClose(t, false)
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(sent, [{ window: 'Find', pid: process.pid, id: '2' }])
})

test('an Android close step whose window stays says it is still open after a close, with no Escape in the text', async (t) => {
  const { r } = await deviceClose(t, true)
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, '"Find" is still open after a close')
})

test('an Android close step that names a window behind another app focuses it by id and stops at the device\'s refusal', async (t) => {
  const mode = 'android-close-named'
  process.env.SHUTTLE_DESK = 'android'
  let device: typeof main
  try { device = ((await import(`../lib/gui_plan.mts?${mode}`)) as { main: typeof main }).main } finally { delete process.env.SHUTTLE_DESK }
  const home = { id: '7', title: 'Home', role: 'window', pid: 4242, active: true }
  const find = { id: '2', title: 'Find', role: 'dialog', pid: process.pid, active: false }
  const refused = 'Find is not in front, and a device brings a window to the front only by launching its app'
  const focused: Record<string, unknown>[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([home, find])
    if (tool === 'desk_focus') { focused.push(args); return { text: refused, error: true } }
    return '{"ok":true}'
  })
  const r = await device({ steps: [{ close: true, window: 'Find' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.deepEqual(focused, [{ window: 'Find', pid: process.pid, id: '2' }])
  assert.match(r.escalate?.reason ?? r.error ?? '', new RegExp(refused))
})
