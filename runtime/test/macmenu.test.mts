import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Untitled', role: 'window', pid: process.pid, active: true }
const nodes = [
  { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Untitled', states: ['showing'] },
  { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
  { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'Edit', states: ['showing'], bounds: [100, 0, 40, 20] },
  { ref: 3, id: '1/4', parent: 2, depth: 3, role: 'menuitem', name: 'Select All', states: [], shortcut: 'cmd+a' },
]

for (const [name, step] of [
  ['on macOS a menu step runs the entry\'s accessibility action, not its keyboard shortcut', { menu: 'Edit > Select All' }],
  ['on macOS a click on a menu item runs the item\'s accessibility action, not its keyboard shortcut', { click: 'Select All' }],
] as const) {
  test(name, { skip: process.platform !== 'darwin' }, async (t) => {
    const calls: { tool: string; args: Record<string, unknown> }[] = []
    await daemon(t, ({ tool, args }) => {
      calls.push({ tool, args })
      if (tool === 'desk_windows') return JSON.stringify([win])
      if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes })
      if (tool === 'desk_press') return '{"ok":true,"how":"action"}'
      return '{"ok":true}'
    })
    const r = await main({ steps: [step] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.ok(calls.some((c) => c.tool === 'desk_press' && c.args.id === '1/4'), JSON.stringify(calls.map((c) => c.tool)))
    assert.ok(!calls.some((c) => c.tool === 'desk_key'), JSON.stringify(calls.filter((c) => c.tool === 'desk_key')))
  })
}

test('on macOS a menu toggle reads its mark after opening its menus, so a stale closed-menu mark neither skips nor repeats the press', { skip: process.platform !== 'darwin' }, async (t) => {
  let on = false
  const pressed: string[] = []
  const tree = () => [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Untitled', states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'Edit', states: ['showing'], bounds: [100, 0, 40, 20] },
    { ref: 3, id: '1/4', parent: 2, depth: 3, role: 'menu', name: 'Spelling and Grammar', states: [] },
    { ref: 4, id: '1/5', parent: 3, depth: 4, role: 'menuitemcheckbox', name: 'Check Spelling While Typing', states: on ? ['checked'] : [] },
  ]
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: tree() })
    if (tool === 'desk_read') return JSON.stringify({ ...tree()[4], states: ['checked'] })
    if (tool === 'desk_press') { pressed.push(String(args.id)); if (args.id === '1/5') on = !on; return '{"ok":true,"how":"action"}' }
    return '{"ok":true}'
  })
  const step = { menu: 'Edit > Spelling and Grammar > Check Spelling While Typing', on: true }
  const first = await main({ steps: [step] })
  assert.equal(first.ok, true, JSON.stringify(first))
  assert.equal(on, true)
  const again = await main({ steps: [step] })
  assert.equal(again.ok, true, JSON.stringify(again))
  assert.equal(on, true)
  assert.equal(pressed.filter((id) => id === '1/5').length, 1, pressed.join(' '))
})
