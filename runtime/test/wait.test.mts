import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

function retitled() {
  const calls: string[] = []
  let title = 'a.txt - Editor'
  const answer = ({ tool, args }: { tool: string; args: Record<string, unknown> }) => {
    calls.push(args.waiting ? 'wait' : tool)
    const win = { id: '1', title, role: 'window', pid: process.pid, active: true }
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: title, states: ['showing'] }] })
    if (tool === 'desk_key') setTimeout(() => { title = '*a.txt - Editor' }, 300)
    if (tool === 'desk_wait') return { text: 'unknown desk tool desk_wait', error: true }
    return '{"ok":true}'
  }
  return { calls, answer }
}

test('a key that retitles the window late waits on the desk for the change instead of polling window lists', async () => {
  const d = retitled()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'ctrl+s' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.equal(r.window, '*a.txt - Editor')
    const after = d.calls.slice(d.calls.indexOf('desk_key'))
    assert.ok(after.includes('wait'), d.calls.join(' '))
    assert.ok(after.filter((c) => c === 'desk_windows').length <= 3, d.calls.join(' '))
  } finally {
    server.close()
  }
})

test('a toolhost without desk_wait gets the window list polled instead', async () => {
  const d = retitled()
  const server = await toolhost(d.answer, { wait: false })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'ctrl+s' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.equal(r.window, '*a.txt - Editor')
    assert.equal(d.calls.filter((c) => c === 'desk_wait').length, 1, d.calls.join(' '))
  } finally {
    server.close()
  }
})
