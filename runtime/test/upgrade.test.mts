import assert from 'node:assert/strict'
import { chmodSync, mkdirSync, statSync, writeFileSync } from 'node:fs'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

test('a flows directory an older version left readable by others turns 0700 on a find or a replay', { skip: process.platform === 'win32' }, async (t) => {
  await toolhost(t, () => '[]')
  const flows = process.env.SHUTTLE_FLOWS ?? ''
  mkdirSync(flows, { recursive: true })
  writeFileSync(`${flows}/old.json`, '{}')
  chmodSync(flows, 0o755)
  await main({ find: 'save the file' })
  assert.equal(statSync(flows).mode & 0o777, 0o700)
  chmodSync(flows, 0o755)
  const missing = await main({ flow: 'nothing' })
  assert.match(missing.escalate?.reason ?? '', /no saved flow/)
  assert.equal(statSync(flows).mode & 0o777, 0o700)
})
