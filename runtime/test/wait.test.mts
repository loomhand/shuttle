import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

function retitled() {
  const calls: string[] = []
  const old = 'a.txt - Editor'
  let title = old
  const answer = ({ tool, args }: { tool: string; args: Record<string, unknown> }) => {
    calls.push(args.waiting ? (title === old ? 'wait' : 'wait-new') : tool)
    const win = { id: '1', title: args.waiting ? title : old, role: 'window', pid: process.pid, active: true }
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: title, states: ['showing'] }] })
    if (tool === 'desk_key') setTimeout(() => { title = '*a.txt - Editor' }, 300)
    return '{"ok":true}'
  }
  return { calls, answer }
}

test('a key that retitles the window late waits on the desk for the change instead of polling window lists', async (t) => {
  const d = retitled()
  await daemon(t, d.answer)
  const r = await main({ steps: [{ key: 'ctrl+s' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  const after = d.calls.slice(d.calls.indexOf('desk_key'))
  assert.equal(after.filter((c) => c.startsWith('wait')).at(-1), 'wait-new', d.calls.join(' '))
  assert.equal(after.filter((c) => c === 'wait-new').length, 1, d.calls.join(' '))
  assert.ok(after.filter((c) => c === 'desk_windows').length <= 3, d.calls.join(' '))
})

test('a wait that several windows behind a known front window match takes none of them', async (t) => {
  const front = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const one = { id: '2', title: 'Report 1 - Viewer', role: 'window', pid: 7, active: false }
  const two = { id: '3', title: 'Report 2 - Viewer', role: 'window', pid: 7, active: false }
  const focused: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([front, one, two])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: front, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: front.title, states: ['showing'] }] })
    if (tool === 'desk_focus') focused.push(args.window)
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ wait: 'Viewer', timeout_ms: 400 }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /2 windows match "Viewer"/)
  assert.deepEqual(focused, [])
})
