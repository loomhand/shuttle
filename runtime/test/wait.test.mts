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

test('a wait asks no model about the front window that only changed its title, and takes the dialog that opens after it', async (t) => {
  const doc = { id: '1', title: 'Untitled - Notepad', role: 'window', pid: process.pid, active: true }
  const dialog = { id: '2', title: 'Save as', role: 'dialog', pid: process.pid, active: true }
  const usage = { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }
  let typed = false; let pressed = 0; let asked = 0; let late = 0
  const real = Date.now; let now = real()
  Date.now = () => now
  t.after(() => { Date.now = real })
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') now += 50
    const open = pressed > 0 && now - pressed > 1000
    if (pressed && !open) late = now - pressed
    const ws = [{ ...doc, title: typed ? `*${doc.title}` : doc.title, active: !open }, ...(open ? [dialog] : [])]
    const w = ws.find((x) => x.active) ?? doc
    if (tool === 'desk_windows') return JSON.stringify(ws)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: w, truncated: false, nodes: [{ ref: 0, id: `${w.id}/1`, parent: -1, depth: 0, role: w.role, name: w.title, states: ['showing'] }] })
    if (tool === 'desk_type') typed = true
    if (tool === 'desk_key') pressed = now
    if (tool === 'model_decide') { asked++; return JSON.stringify({ answers: { is: { probability: 0.1 } }, served: { model: 'm', build: null, id: null }, usage }) }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ type: 'hi' }, { key: 'ctrl+s' }, { wait: 'Save as' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, 'Save as')
  assert.equal(asked, 0)
  assert.ok(late > 500, String(late))
})
