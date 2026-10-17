import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('an app step finds a running app by the app its windows name, whatever their titles', async () => {
  const doc = { id: '1', title: 'Untitled', role: 'window', pid: process.pid, active: false, app: 'TextEdit' }
  const calls: string[] = []
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([doc])
    if (tool === 'desk_focus') {
      if (args.window !== doc.title) return { text: `no window "${String(args.window)}"`, error: true }
      doc.active = true
      return JSON.stringify({ ok: true, title: doc.title, pid: doc.pid })
    }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ app: 'TextEdit' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.equal(r.window, 'Untitled')
    assert.ok(!calls.includes('desk_launch'), calls.join(' '))
  } finally {
    server.close()
  }
})
