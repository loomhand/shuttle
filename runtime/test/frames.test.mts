import assert from 'node:assert/strict'
import { mkdtempSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { setTimeout as sleep } from 'node:timers/promises'
import { test } from 'node:test'

import { endpoint } from './toolhost.mts'

test('a reply split over several writes arrives whole, and a connection closed mid-frame rejects', async () => {
  const body = Buffer.from(JSON.stringify({ text: JSON.stringify({ width: 1280, height: 800 }), is_error: false }))
  const head = Buffer.alloc(4)
  head.writeUInt32BE(body.length)
  let whole = true
  const server = createServer((s) => {
    s.once('data', async () => {
      if (!whole) { s.end(Buffer.concat([head, body.subarray(0, 5)])); return }
      s.write(head.subarray(0, 2)); await sleep(10)
      s.write(Buffer.concat([head.subarray(2), body.subarray(0, 7)])); await sleep(10)
      s.end(body.subarray(7))
    })
  })
  const socket = endpoint(mkdtempSync(`${tmpdir()}/frames-`))
  await new Promise<void>((r) => server.listen(socket, r))
  process.env.TOOLHOST_SOCKET = socket
  const desk = await import('../lib/desk.mts')
  try {
    assert.deepEqual(await desk.call('screen'), { width: 1280, height: 800 })
    whole = false
    await assert.rejects(desk.call('screen'), /closed the connection mid-frame/)
  } finally {
    server.close()
  }
})
