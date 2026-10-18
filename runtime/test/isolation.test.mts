import assert from 'node:assert/strict'
import { spawnSync } from 'node:child_process'
import { existsSync, statSync } from 'node:fs'
import { homedir, tmpdir } from 'node:os'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

test('a plan, a save and a reindex write flows, the index, the pending chain and git only under the test directory', async (t) => {
  const names = ['.runs.jsonl', '.pending.json', 'INDEX', 'mousepad.json', 'saved.json', '.git']
  const real = [`${homedir()}/workspace/flows`, '/workspace/flows'].flatMap((d) => names.map((n) => `${d}/${n}`))
  const stamp = () => real.map((f) => existsSync(f) ? statSync(f).mtimeMs : null)
  const before = stamp()
  const win = { id: '1', title: 'notes.txt - Mousepad', role: 'frame', pid: process.pid, active: true }
  const server = await toolhost(({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  t.after(() => server.close())
  const ran = await main({ steps: [{ app: 'Mousepad' }, { key: 'ctrl+s' }] })
  assert.equal(ran.ok, true, JSON.stringify(ran))
  const saved = await main({ save: 'saved' })
  assert.equal(saved.ok, true, JSON.stringify(saved))
  assert.equal((await main({ reindex: true })).ok, true)
  const flows = process.env.SHUTTLE_FLOWS ?? ''
  assert.ok(flows.startsWith(tmpdir()), flows)
  for (const n of ['.runs.jsonl', '.pending.json', 'INDEX', 'saved.json']) assert.ok(existsSync(`${flows}/${n}`), n)
  assert.ok(!existsSync(`${flows}/mousepad.json`))
  const tools = process.platform !== 'darwin' || spawnSync('xcode-select', ['-p']).status === 0
  if (tools && spawnSync('git', ['--version']).status === 0) assert.ok(existsSync(`${flows}/.git`))
  if (process.platform !== 'win32') {
    assert.equal(statSync(flows).mode & 0o777, 0o700)
    for (const n of ['.runs.jsonl', '.pending.json', 'INDEX', 'saved.json']) assert.equal(statSync(`${flows}/${n}`).mode & 0o777, 0o600, n)
  }
  assert.deepEqual(stamp(), before)
})
