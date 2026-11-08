import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Untitled', role: 'window', pid: process.pid, active: true }

test('on macOS a menu step rereads the menu after opening it, so an entry that retitled itself matches its new title', { skip: process.platform !== 'darwin' }, async (t) => {
  let validated = false
  const pressed: string[] = []
  const keys: unknown[] = []
  const tree = () => [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Untitled', states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'Format', states: ['showing'], bounds: [100, 0, 50, 20] },
    { ref: 3, id: '1/4', parent: 2, depth: 3, role: 'menuitem', name: validated ? 'Wrap to Window' : 'Wrap to Page', states: [] },
  ]
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: tree() })
    if (tool === 'desk_press') { pressed.push(String(args.id)); if (args.id === '1/3') validated = true; return '{"ok":true,"how":"action"}' }
    if (tool === 'desk_key') { keys.push(args.keys); return '{"ok":true}' }
    if (tool === 'model_ask') return { text: 'no model', error: true }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ menu: 'Format > Wrap to Window' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(pressed, ['1/3', '1/4'])
  assert.deepEqual(keys, ['Escape'])
})

test('on macOS a submenu whose id changes once its parent opens is opened again from a fresh tree, and every opened menu is closed', { skip: process.platform !== 'darwin' }, async (t) => {
  let parentOpened = false
  let validated = false
  let open = 0
  const pressed: string[] = []
  const sub = () => parentOpened ? '1/9' : '1/4'
  const tree = () => [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Untitled', states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'Format', states: ['showing'], bounds: [100, 0, 50, 20] },
    { ref: 3, id: sub(), parent: 2, depth: 3, role: 'menu', name: 'Text', states: [] },
    { ref: 4, id: '1/5', parent: 3, depth: 4, role: 'menuitem', name: validated ? 'Hide Ruler' : 'Show Ruler', states: [] },
  ]
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: tree() })
    if (tool === 'desk_press') {
      const id = String(args.id); pressed.push(id)
      if (id === '1/3') { parentOpened = true; open++ }
      else if (id === '1/4' || id === '1/9') { if (id !== sub()) return { text: `element ${id} is gone`, error: true }; validated = true; open++ }
      return '{"ok":true,"how":"action"}'
    }
    if (tool === 'desk_key') { if (args.keys === 'Escape') open = Math.max(0, open - 1); return '{"ok":true}' }
    if (tool === 'model_ask') return { text: 'no model', error: true }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ menu: 'Format > Text > Hide Ruler' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(pressed, ['1/3', '1/4', '1/3', '1/9', '1/5'])
  assert.equal(open, 0)
})
