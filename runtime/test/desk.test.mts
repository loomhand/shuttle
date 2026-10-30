import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
import { test } from 'node:test'

import { BUILD, daemon } from './daemon.mts'
import * as desk from '../lib/desk.mts'

test('ask sends one PNG buffer or a list of them as base64 and meters the usage by role into the run', async (t) => {
  const sent: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool !== 'model_ask') return { text: `unexpected ${tool}`, error: true }
    sent.push(args.images)
    return JSON.stringify({ text: 'ok', usage: { model: 'vision-1', input_tokens: 10, output_tokens: 2, cached_tokens: 0 } })
  })
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  t.after(() => { delete process.env.SHUTTLE_RUN })
  const png = Buffer.from('png')
  assert.equal(await desk.ask('one', png), 'ok')
  assert.equal(await desk.ask('two', [png, png]), 'ok')
  assert.equal(await desk.ask('none'), 'ok')
  const b64 = png.toString('base64')
  assert.deepEqual(sent, [[b64], [b64, b64], []])
  assert.deepEqual(desk.usage.ask, { calls: 3, input_tokens: 30, output_tokens: 6, cached_tokens: 0 })
  assert.equal(desk.usage['vision-1'], undefined)
  assert.deepEqual(JSON.parse(readFileSync(`${run}/usage.json`, 'utf8')), desk.usage)
})

test('a decision is served and metered by role, with the build as its date tag and no model name', async (t) => {
  await daemon(t, ({ tool }) => {
    if (tool !== 'model_decide') return { text: `unexpected ${tool}`, error: true }
    return JSON.stringify({
      answers: { ok: { probability: 0.9 } },
      served: { model: 'typesafe/jev-1.14', build: BUILD, id: 'gen-1' },
      usage: { model: 'typesafe/jev-1.14', input_tokens: 5, output_tokens: 1, cached_tokens: 0 },
    })
  })
  const before = desk.served.length
  await desk.decide('state', { ok: { type: 'noul', instructions: 'ok?' } })
  assert.deepEqual(desk.served.slice(before), [{ model: 'decide', image: false, build: '20261014', id: 'gen-1' }])
  assert.equal(desk.usage.decide?.calls, 1)
  assert.doesNotMatch(JSON.stringify({ served: desk.served, usage: desk.usage }), /typesafe|jev/)
})
