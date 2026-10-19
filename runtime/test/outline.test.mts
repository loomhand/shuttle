import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Big - Editor', role: 'window', pid: process.pid, active: true }

function escalation(t: { after(fn: () => void): void }, truncated: boolean, buttons: number) {
  const nodes = [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing', 'active'] }]
  for (let i = 1; i <= buttons; i++) nodes.push({ ref: i, id: `1/${i + 1}`, parent: 0, depth: 1, role: 'button', name: `Toolbar action number ${i}`, states: ['showing'] })
  return daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated, nodes })
    return '{"ok":true}'
  })
}

test('an outline cut by the backend says this read missed the rest and narrowing the view brings it in', async (t) => {
  await escalation(t, true, 3)
  const r = await main({ steps: [{ check: 'Nope' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.outline ?? '', /accessibility read stopped before the end of the window/)
  assert.doesNotMatch(r.escalate?.outline ?? '', /desk snapshot or find reads the rest/)
})

test('an outline cut only by its text budget points to desk snapshot or find', async (t) => {
  await escalation(t, false, 200)
  const r = await main({ steps: [{ check: 'Nope' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.outline ?? '', /desk snapshot or find reads the rest/)
  assert.doesNotMatch(r.escalate?.outline ?? '', /accessibility read stopped/)
})

test('a complete outline gets no cut note', async (t) => {
  await escalation(t, false, 3)
  const r = await main({ steps: [{ check: 'Nope' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.doesNotMatch(r.escalate?.outline ?? '', /reads the rest|accessibility read stopped/)
})
