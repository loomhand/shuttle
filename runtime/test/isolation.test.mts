import assert from 'node:assert/strict'
import { existsSync, statSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('a plan writes its flows and run log under the test directory only, never in the real home or /workspace', async () => {
  const real = [`${homedir()}/workspace/flows/.runs.jsonl`, '/workspace/flows/.runs.jsonl']
  const stamp = () => real.map((f) => existsSync(f) ? statSync(f).mtimeMs : null)
  const before = stamp()
  const win = { id: '1', title: 'notes.txt - Mousepad', role: 'frame', pid: process.pid, active: true }
  const server = await toolhost(({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  const r = await main({ steps: [{ app: 'Mousepad' }, { key: 'ctrl+s' }] })
  server.close()
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.ok(process.env.SHUTTLE_FLOWS?.startsWith(tmpdir()))
  assert.ok(existsSync(`${process.env.SHUTTLE_FLOWS}/.runs.jsonl`))
  assert.deepEqual(stamp(), before)
})
