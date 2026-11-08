import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('a flow commit is never signed, so a user signing setup neither prompts nor loses the commit', async (t) => {
  const dir = mkdtempSync(`${tmpdir()}/gitsign-`)
  writeFileSync(`${dir}/config`, '[commit]\n\tgpgsign = true\n[gpg]\n\tprogram = false\n')
  const was = process.env.GIT_CONFIG_GLOBAL
  process.env.GIT_CONFIG_GLOBAL = `${dir}/config`
  t.after(() => { if (was === undefined) delete process.env.GIT_CONFIG_GLOBAL; else process.env.GIT_CONFIG_GLOBAL = was })
  const notepad = { id: '1', title: 'a.txt - Notepad', role: 'window', pid: process.pid, active: true }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }] })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'hello' }] })
  assert.equal(r.saved, 'notepad', JSON.stringify(r))
  const log = execFileSync('git', ['-C', process.env.SHUTTLE_FLOWS ?? '', 'log', '--format=%s'], { encoding: 'utf8' })
  assert.match(log, /^notepad: saved from 2 passing steps$/m)
})
