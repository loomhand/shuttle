import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { test } from 'node:test'
import { setTimeout as sleep } from 'node:timers/promises'

import { daemon } from './daemon.mts'

test('a plan killed outright leaves the steps it finished as its result', { skip: process.platform === 'win32' }, async (t) => {
  const win = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] }] })
    return '{"ok":true}'
  })
  const run = mkdtempSync(`${tmpdir()}/run-`)
  writeFileSync(`${run}/params.json`, JSON.stringify({ steps: [{ key: 'ctrl+b' }, { wait: 'Never', timeout_ms: 30000 }] }))
  const lib = new URL('../lib/', import.meta.url).pathname
  const child = spawn(process.execPath, [`${lib}entry.mts`], { env: { ...process.env, SHUTTLE_SCRIPT: `${lib}gui_plan.mts`, SHUTTLE_RUN: run }, stdio: 'ignore' })
  const ended = new Promise((r) => child.on('exit', r))
  for (let i = 0; i < 500 && finished(run) !== 1; i++) await sleep(10)
  child.kill('SIGKILL')
  await ended
  const r = JSON.parse(readFileSync(`${run}/result.json`, 'utf8')) as { ok: boolean; done: number; trace: { op: string }[] }
  assert.equal(r.ok, false)
  assert.equal(r.done, 1)
  assert.deepEqual(r.trace.map((s) => s.op), ['key'])
})

function finished(run: string): number | undefined {
  try { return (JSON.parse(readFileSync(`${run}/result.json`, 'utf8')) as { done?: number }).done } catch { return undefined }
}
