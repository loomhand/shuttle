import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('a step that typed into an empty field stops when its app was launched and reopened its last document', async () => {
  let launched = false
  const typed: string[] = []
  const win = { id: '1', title: 'alice.txt - Mousepad', role: 'frame', pid: process.pid, active: true }
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(launched ? [win] : [])
    if (tool === 'desk_focus') return launched ? JSON.stringify({ ok: true, title: win.title, pid: win.pid }) : { text: 'no such window', error: true }
    if (tool === 'desk_launch') launched = true
    if (tool === 'desk_type') typed.push(String(args.text))
    if (tool === 'desk_snapshot') {
      return JSON.stringify({
        window: win, truncated: false,
        nodes: [
          { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'frame', name: win.title, states: ['showing', 'active'] },
          { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused'], value: 'alice lives in Lisbon' },
        ],
      })
    }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ app: 'mousepad' }, { type: 'bob lives in Oslo', blank: { role: 'textbox', name: '', parent: '', nth: 0 } }] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate?.reason ?? '', /already holds text/)
    assert.deepEqual(typed, [])
  } finally {
    server.close()
  }
})
