import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('windows the desk marks as the desktop shell stay out of the windows an escalation lists', async (t) => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const panel = { id: '2', title: 'Whisker Menu', role: 'window', pid: 7, active: false, shell: true }
  const desktop = { id: '3', title: 'Desktop', role: 'window', pid: 8, active: false, shell: true }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([doc, panel, desktop])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }] })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'Nope' }] })
  assert.ok(r.escalate, JSON.stringify(r))
  assert.deepEqual(r.escalate.windows, ['a.txt - Editor'])
})
