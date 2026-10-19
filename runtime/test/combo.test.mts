import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('set on a combo box without a text field picks the option from its list, does nothing when it is already chosen, and names the options when none matches', async (t) => {
  const prefs = { id: '1.14/322', title: 'Mousepad Preferences', role: 'dialog', pid: 889, active: true }
  const list = { id: '1.14/537', title: '', role: 'window', pid: 889, active: true }
  let open = false
  const nodes = [
    { ref: 0, id: '1.14/322', parent: -1, depth: 0, role: 'dialog', name: 'Mousepad Preferences', states: ['showing'] },
    { ref: 1, id: '1.14/400', parent: 0, depth: 1, role: 'combobox', name: 'Insert Tabs', states: ['showing'], bounds: [247, 197, 137, 34] },
    { ref: 2, id: '1.14/401', parent: 1, depth: 2, role: 'menu', name: '', states: [] },
    { ref: 3, id: '1.14/402', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Tabs', states: ['visible'] },
    { ref: 4, id: '1.14/403', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Spaces', states: ['visible'] },
  ]
  const now = () => nodes.map((n) => ({ ...n, states: n.depth === 3 && open ? ['visible', 'showing'] : n.states }))
  const pressed: string[] = []
  const typed: string[] = []
  const combo = nodes[1]
  assert.ok(combo)
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(open ? [list, { ...prefs, active: false }] : [prefs])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: prefs, nodes: now(), truncated: false })
    if (tool === 'desk_read') return JSON.stringify(now().find((n) => n.id === args.id))
    if (tool === 'desk_type') typed.push(String(args.text))
    if (tool === 'desk_press') {
      pressed.push(String(args.id))
      const item = nodes.find((n) => n.id === args.id && n.role === 'menuitem')
      if (item && open) combo.name = item.name
      open = args.id === '1.14/400'
      return '{"ok":true,"how":"click"}'
    }
    return '{"ok":true}'
  })
  const set = await main({ steps: [{ set: 'Insert Tabs', value: 'Insert Spaces' }] })
  assert.equal(set.ok, true, JSON.stringify(set))
  assert.deepEqual(pressed, ['1.14/400', '1.14/403'])
  assert.equal(combo.name, 'Insert Spaces')
  assert.deepEqual(typed, [])
  const again = await main({ steps: [{ set: 'Insert Spaces', value: 'Insert Spaces' }] })
  assert.equal(again.ok, true, JSON.stringify(again))
  assert.equal(again.trace?.[0]?.noop, true, JSON.stringify(again))
  assert.equal(pressed.length, 2)
  const none = await main({ steps: [{ set: 'Insert Spaces', value: 'Insert Nothing' }] })
  assert.equal(none.escalate?.reason, '"Insert Spaces" has no option "Insert Nothing"; its options: "Insert Tabs", "Insert Spaces"', JSON.stringify(none))
  assert.equal(pressed.length, 2)
})
