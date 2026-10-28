import assert from 'node:assert/strict'
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('a step that typed into an empty field stops when its app was launched and reopened its last document', async (t) => {
  let launched = false
  const typed: string[] = []
  const win = { id: '1', title: 'alice.txt - Mousepad', role: 'frame', pid: process.pid, active: true }
  await daemon(t, ({ tool, args }) => {
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
  const r = await main({ steps: [{ app: 'mousepad' }, { type: 'bob lives in Oslo', blank: { role: 'textbox', name: '', parent: '', nth: 0 } }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /already holds text/)
  assert.deepEqual(typed, [])
})

test('a plan that continues a flow from the step it stopped at saves the fixed steps into that flow', async (t) => {
  const win = { id: '1', title: 'run-0.txt - Mousepad', role: 'frame', pid: process.pid, active: true }
  let text = 'replay run 0'
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_focus') return JSON.stringify({ ok: true, title: win.title, pid: win.pid })
    if (tool === 'desk_key' && args.keys === 'ctrl+n') text = ''
    if (tool === 'desk_type') text += String(args.text)
    if (tool === 'desk_snapshot') {
      return JSON.stringify({
        window: win, truncated: false,
        nodes: [
          { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'frame', name: win.title, states: ['showing', 'active'] },
          { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused'], value: text },
        ],
      })
    }
    return '{"ok":true}'
  })
  const dir = process.env.SHUTTLE_FLOWS ?? ''
  const blank = { role: 'textbox', name: '', parent: '', nth: 0 }
  mkdirSync(dir, { recursive: true })
  writeFileSync(`${dir}/note.json`, JSON.stringify({ name: 'note', params: { n: '0' }, steps: [{ app: 'mousepad' }, { type: 'replay run {{n}}', blank }], idempotent: false, runs: 1, saved_at: '2026-10-29T00:00:00.000Z', rewrites: 0 }))
  const stopped = await main({ flow: 'note', params: { n: '1' } })
  assert.equal(stopped.escalate?.step, 1, JSON.stringify(stopped))
  const healed = await main({ steps: [{ key: 'ctrl+n' }, { type: 'replay run {{n}}' }], params: { n: '1' } })
  assert.equal(healed.saved, 'note', JSON.stringify(healed))
  const saved = JSON.parse(readFileSync(`${dir}/note.json`, 'utf8')) as { params: Record<string, string>; steps: Record<string, unknown>[] }
  assert.deepEqual(saved.steps.map((s) => s.app ?? s.key ?? s.type), ['mousepad', 'ctrl+n', 'replay run {{n}}'])
  assert.equal(saved.params.n, '1')
})
