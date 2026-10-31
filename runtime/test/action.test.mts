import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main as plan } from '../lib/gui_plan.mts'

test('a Linux step that only ran an accessibility action carries a caveat that no later step read its effect, and does not escalate', { skip: process.platform !== 'linux' }, async (t) => {
  const win = { id: '1.14/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 889, active: true }
  const nodes = [
    { ref: 0, id: '1.14/117', parent: -1, depth: 0, role: 'window', name: 'Untitled 1 - Mousepad', states: ['showing'] },
    { ref: 1, id: '1.14/400', parent: 0, depth: 1, role: 'button', name: 'Save', states: ['showing'], bounds: [420, 300, 80, 30] },
  ]
  let how = 'action'
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes, truncated: false })
    if (tool === 'desk_press') return JSON.stringify({ ok: true, how })
    return '{"ok":true}'
  })
  const blind = await plan({ steps: [{ click: 'Save' }] })
  assert.equal(blind.ok, true, JSON.stringify(blind))
  assert.equal(blind.escalate, undefined)
  assert.deepEqual((blind.caveats as { step: number }[]).map((c) => c.step), [0], JSON.stringify(blind))
  const read = await plan({ steps: [{ click: 'Save' }, { verify: 'Save' }] })
  assert.equal(read.ok, true, JSON.stringify(read))
  assert.equal(read.caveats, undefined, JSON.stringify(read))
  how = 'select'
  const selected = await plan({ steps: [{ click: 'Save' }] })
  assert.deepEqual((selected.caveats as { step: number }[]).map((c) => c.step), [0], JSON.stringify(selected))
  how = 'click'
  const clicked = await plan({ steps: [{ click: 'Save' }] })
  assert.equal(clicked.ok, true, JSON.stringify(clicked))
  assert.equal(clicked.caveats, undefined, JSON.stringify(clicked))
})
