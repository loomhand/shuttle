import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import * as desk from '../lib/desk.mts'

test('ask sends one PNG buffer or a list of them as base64 and meters the usage by model', async (t) => {
  const sent: unknown[] = []
  await toolhost(t, ({ tool, args }) => {
    if (tool !== 'model_ask') return { text: `unexpected ${tool}`, error: true }
    sent.push(args.images)
    return JSON.stringify({ text: 'ok', usage: { model: 'vision-1', input_tokens: 10, output_tokens: 2, cached_tokens: 0 } })
  })
  const png = Buffer.from('png')
  assert.equal(await desk.ask('one', png), 'ok')
  assert.equal(await desk.ask('two', [png, png]), 'ok')
  assert.equal(await desk.ask('none'), 'ok')
  const b64 = png.toString('base64')
  assert.deepEqual(sent, [[b64], [b64, b64], []])
  assert.deepEqual(desk.usage['vision-1'], { calls: 3, input_tokens: 30, output_tokens: 6, cached_tokens: 0 })
})
