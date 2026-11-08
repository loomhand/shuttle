import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

for (const stalled of [true, false]) {
  test(`a menu shortcut ${stalled ? 'stalled after sending stops without another action' : 'refused before sending falls back to its action'}`, { skip: process.platform === 'darwin' }, async (t) => {
    const win = { id: '1', title: 'Untitled', role: 'window', pid: process.pid, active: true }
    const nodes = [
      { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] },
      { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menu', name: 'Edit', states: ['showing'] },
      { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menuitem', name: 'Undo', states: ['showing'], shortcut: 'ctrl+z' },
    ]
    const error = stalled ? 'hotkey: input_delivery_stalled: input was inserted and may still appear in the target' : 'hotkey: foreground_unavailable: no input was sent'
    const calls: string[] = []
    await daemon(t, ({ tool, args }) => {
      if (tool === 'desk_windows') return JSON.stringify([win])
      if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes })
      if (tool === 'desk_key') { calls.push(String(args.keys)); if (args.keys === 'ctrl+z') return { text: error, error: true } }
      if (tool === 'desk_press') { calls.push(String(args.id)); return '{"ok":true,"how":"action"}' }
      return '{"ok":true}'
    })
    const r = await main({ steps: [{ click: 'Undo' }] })
    assert.equal(r.ok, !stalled, JSON.stringify(r))
    if (stalled) assert.equal(r.escalate?.reason, error)
    assert.deepEqual(calls, stalled ? ['Escape', 'ctrl+z'] : ['Escape', 'ctrl+z', '1/3'])
  })
}

test('a menu shortcut the user declines stops the plan without a press of the entry', { skip: process.platform === 'darwin' }, async (t) => {
  const win = { id: '1', title: 'Untitled', role: 'window', pid: process.pid, active: true }
  const nodes = [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menu', name: 'Edit', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menuitem', name: 'Undo', states: ['showing'], shortcut: 'ctrl+z' },
  ]
  const calls: string[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes })
    if (tool === 'desk_key') { calls.push(String(args.keys)); if (args.keys === 'ctrl+z') return { text: 'declined by the user', error: true } }
    if (tool === 'desk_press') { calls.push(String(args.id)); return '{"ok":true,"how":"action"}' }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'Undo' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, 'declined by the user')
  assert.deepEqual(calls, ['Escape', 'ctrl+z'])
})
