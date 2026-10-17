import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('a step whose target is missing waits for the window the step before opened, then runs there', async () => {
  const doc = { id: '1', title: 'a.txt - Notepad', role: 'window', pid: process.pid, active: true }
  const dialog = { id: '2', title: 'Save as', role: 'dialog', pid: process.pid, active: true }
  let pressed = 0; let value = ''; let selected = false; let saved = false
  const open = () => pressed > 0 && Date.now() - pressed > 600 && !saved
  const field = () => ({ ref: 1, id: '2/2', parent: 0, depth: 1, role: 'textbox', name: 'File name:', states: ['showing', 'focused'], bounds: [10, 10, 100, 20], value })
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(open() ? [{ ...doc, active: false }, dialog] : [doc])
    if (tool === 'desk_snapshot' && args.window === 'Save as') return JSON.stringify({ window: dialog, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Save as', states: ['showing'] }, field()] })
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }, { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'Save as', states: ['showing'], bounds: [10, 10, 60, 20] }] })
    if (tool === 'desk_read') return JSON.stringify(field())
    if (tool === 'desk_press') { pressed ||= Date.now(); return '{"ok":true,"how":"action"}' }
    if (tool === 'desk_key' && /^(cmd|ctrl)\+a$/.test(String(args.keys))) selected = true
    if (tool === 'desk_key' && /^return$/i.test(String(args.keys))) saved = true
    if (tool === 'desk_type') { value = selected ? String(args.text) : value + String(args.text); selected = false }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ click: 'Save as' }, { set: 'File name:', value: 'C:\\a.txt' }, { key: 'Return' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.equal(value, 'C:\\a.txt')
    assert.equal(saved, true)
  } finally {
    server.close()
  }
})
