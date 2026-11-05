import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Format', role: 'dialog', pid: process.pid, active: true }
const root = { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'dialog', name: 'Format', states: ['showing'] }

test('a verify that finds no element fails, whatever state it asks for', async (t) => {
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    return '{"ok":true}'
  })
  for (const step of [{ verify: 'Bold', on: false }, { verify: 'Bold', enabled: false }]) {
    const r = await main({ steps: [step] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate?.reason ?? '', /no element matches "Bold"/)
  }
})
