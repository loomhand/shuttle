import assert from 'node:assert/strict'
import { chmodSync, existsSync, mkdtempSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('on macOS without the Command Line Tools a flow never runs git, which would open their installer', { skip: process.platform !== 'darwin' }, async (t) => {
  const bin = mkdtempSync(`${tmpdir()}/clt-`)
  const ran = `${bin}/git-ran`
  writeFileSync(`${bin}/git`, `#!/bin/sh\n/usr/bin/touch ${ran}\n`)
  writeFileSync(`${bin}/xcode-select`, '#!/bin/sh\nexit 2\n')
  chmodSync(`${bin}/git`, 0o755)
  chmodSync(`${bin}/xcode-select`, 0o755)
  const path = process.env.PATH
  process.env.PATH = bin
  t.after(() => { process.env.PATH = path })
  const notepad = { id: '1', title: 'a.txt - Notepad', role: 'window', pid: process.pid, active: true }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }] })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'Notepad' }, { type: 'hello' }] })
  assert.equal(r.saved, 'notepad', JSON.stringify(r))
  assert.ok(!existsSync(ran), 'git ran')
})
