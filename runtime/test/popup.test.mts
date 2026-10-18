import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main as plan } from '../lib/gui_plan.mts'

test('a click that opens a combo box\'s list leaves the next step in the dialog, whose tree holds the list\'s entries, also in a new plan', async (t) => {
  const wm = { id: '1.3/1', title: '', role: 'window', pid: 687, active: true }
  const main = { id: '1.14/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 889, active: false }
  const prefs = { id: '1.14/322', title: 'Mousepad Preferences', role: 'dialog', pid: 889, active: true }
  const list = { id: '1.14/537', title: '', role: 'window', pid: 889, active: true }
  let open = false
  const nodes = [
    { ref: 0, id: '1.14/322', parent: -1, depth: 0, role: 'dialog', name: 'Mousepad Preferences', states: ['showing'] },
    { ref: 1, id: '1.14/400', parent: 0, depth: 1, role: 'combobox', name: 'Insert Tabs', states: ['showing'], bounds: [420, 300, 160, 30] },
    { ref: 2, id: '1.14/401', parent: 1, depth: 2, role: 'menu', name: '', states: [] },
    { ref: 3, id: '1.14/402', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Tabs', states: [], bounds: [420, 300, 160, 26] },
    { ref: 4, id: '1.14/403', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Spaces', states: [], bounds: [420, 326, 160, 26] },
  ]
  const pressed: string[] = []
  const snapped: string[] = []
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(open ? [wm, list, main, { ...prefs, active: false }] : [wm, main, prefs])
    if (tool === 'desk_snapshot') {
      snapped.push(String(args.window))
      const shown = nodes.map((n) => ({ ...n, states: n.depth === 3 && open ? ['showing'] : n.states }))
      return JSON.stringify({ window: prefs, nodes: args.window === prefs.title ? shown : [], truncated: false })
    }
    if (tool === 'desk_press') { pressed.push(String(args.id)); open = args.id === '1.14/400'; return '{"ok":true,"how":"click"}' }
    return '{"ok":true}'
  })
  t.after(() => server.close())
  const both = await plan({ steps: [{ click: 'Insert Tabs' }, { click: 'Insert Spaces' }] })
  assert.equal(both.ok, true, JSON.stringify(both))
  assert.deepEqual(pressed, ['1.14/400', '1.14/403'])
  assert.equal(both.window, 'Mousepad Preferences')
  const left = await plan({ steps: [{ click: 'Insert Tabs' }] })
  assert.equal(left.ok, true, JSON.stringify(left))
  const next = await plan({ steps: [{ click: 'Insert Spaces' }] })
  assert.equal(next.ok, true, JSON.stringify(next))
  assert.deepEqual(pressed.slice(2), ['1.14/400', '1.14/403'])
  assert.ok(snapped.every((w) => w === 'Mousepad Preferences'), snapped.join(' '))
})
