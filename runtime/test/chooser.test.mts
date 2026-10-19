import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

function desktop(role: string) {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const front = { id: '2', title: role === 'dialog' ? 'Save As' : 'Sign up - Browser', role, pid: process.pid, active: true }
  const calls: string[] = []
  let open = true
  const places = Array.from({ length: 24 }, (_, i) => ({ ref: i + 1, id: `2/${i + 2}`, parent: 0, depth: 1, role: 'button', name: `Place ${i} ${'x'.repeat(90)}`, states: ['showing'] }))
  const fields = [
    { role: 'text', name: 'Save As:' },
    { role: 'combobox', name: 'File name:' },
    { role: 'textbox', name: 'File Chooser Widget' },
  ].map((f, i) => ({ ref: 25 + i, id: `2/${30 + i}`, parent: 0, depth: 1, states: ['showing'], ...f }))
  const answer = ({ tool, args }: { tool: string; args: Record<string, unknown> }) => {
    calls.push(tool === 'desk_key' ? `key ${String(args.keys)}` : tool)
    if (tool === 'desk_windows') return JSON.stringify(open ? [doc, front] : [{ ...doc, active: true }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: front, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role, name: front.title, states: ['showing'] }, ...places, ...fields] })
    if (tool === 'desk_key' && /^return$/i.test(String(args.keys)) && role === 'dialog') open = false
    return '{"ok":true}'
  }
  return { calls, answer }
}

test('a typed path finds the file chooser by its name field even when the field sits past the outline\'s first 1800 characters', async (t) => {
  const d = desktop('dialog')
  await daemon(t, d.answer)
  const r = await main({ steps: [{ type: 'b.txt\n' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  const before = d.calls.slice(0, d.calls.indexOf('desk_type'))
  assert.ok(before.some((c) => c.startsWith('key ')), `no select key before typing into the chooser: ${before.join(' ')}`)
})

test('Enter in a file chooser whose name field sits past the outline\'s first 1800 characters waits for its button before it submits', async (t) => {
  const d = desktop('dialog')
  await daemon(t, d.answer)
  const r = await main({ steps: [{ key: 'Return' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  const before = d.calls.slice(0, d.calls.indexOf('key Return'))
  assert.equal(before.filter((c) => c === 'desk_snapshot').length, 2, before.join(' '))
})

test('a typed line into a large window that is no dialog but holds a File name field goes in as plain typing with one Return', async (t) => {
  const d = desktop('window')
  await daemon(t, d.answer)
  await main({ steps: [{ type: 'b.txt\n' }] })
  const before = d.calls.slice(0, d.calls.indexOf('desk_type'))
  assert.ok(!before.some((c) => c.startsWith('key ')), `a select key before plain typing: ${before.join(' ')}`)
  assert.equal(d.calls.filter((c) => /^key return$/i.test(c)).length, 1, d.calls.join(' '))
})
