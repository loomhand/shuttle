import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

test('a field named like the combobox it sits in is one target, and set needs no model to pick it', async (t) => {
  const dialog = { id: '2', title: 'Save as', role: 'dialog', pid: process.pid, active: true }
  let value = ''; let selected = false
  const calls: string[] = []
  const nodes = () => [
    { ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Save as', states: ['showing'] },
    { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'combobox', name: 'File name:', states: ['showing', 'focusable'], bounds: [10, 10, 200, 20], value },
    { ref: 2, id: '2/3', parent: 1, depth: 2, role: 'textbox', name: 'File name:', states: ['showing', 'focused', 'editable'], bounds: [12, 12, 180, 16], value },
  ]
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([dialog])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: dialog, truncated: false, nodes: nodes() })
    if (tool === 'desk_read') return JSON.stringify(nodes().find((n) => n.id === args.id))
    if (tool.startsWith('model_')) return { text: 'no model in this test', error: true }
    if (tool === 'desk_key' && /^(cmd|ctrl)\+a$/.test(String(args.keys))) selected = true
    if (tool === 'desk_type') { value = selected ? String(args.text) : value + String(args.text); selected = false }
    return '{"ok":true}'
  })
  t.after(() => server.close())
  const r = await main({ steps: [{ set: 'File name:', value: 'a.txt' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(value, 'a.txt')
  assert.ok(!calls.some((c) => c.startsWith('model_')), calls.join(' '))
})
