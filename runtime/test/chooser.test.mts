import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('a typed path finds the file chooser by its name field even when the field sits past the outline\'s first 1800 characters', async () => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const dialog = { id: '2', title: 'Save As', role: 'dialog', pid: process.pid, active: true }
  const calls: string[] = []
  let open = true
  const places = Array.from({ length: 24 }, (_, i) => ({ ref: i + 1, id: `2/${i + 2}`, parent: 0, depth: 1, role: 'button', name: `Place ${i} ${'x'.repeat(90)}`, states: ['showing'] }))
  const fields = [
    { role: 'text', name: 'Save As:' },
    { role: 'combobox', name: 'File name:' },
    { role: 'textbox', name: 'File Chooser Widget' },
  ].map((f, i) => ({ ref: 25 + i, id: `2/${30 + i}`, parent: 0, depth: 1, states: ['showing'], ...f }))
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool === 'desk_key' ? `key ${String(args.keys)}` : tool)
    if (tool === 'desk_windows') return JSON.stringify(open ? [doc, dialog] : [{ ...doc, active: true }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: dialog, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Save As', states: ['showing'] }, ...places, ...fields] })
    if (tool === 'desk_key' && /^return$/i.test(String(args.keys))) open = false
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    await main({ steps: [{ type: 'b.txt\n' }] })
    const before = calls.slice(0, calls.indexOf('desk_type'))
    assert.ok(before.some((c) => c.startsWith('key ')), `no select key before typing into the chooser: ${before.join(' ')}`)
  } finally {
    server.close()
  }
})
