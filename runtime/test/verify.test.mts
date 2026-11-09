import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon, gray } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Format', role: 'dialog', pid: process.pid, active: true }
const root = { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'dialog', name: 'Format', states: ['showing'] }
const usage = { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }

test('a verify that finds no element fails, whatever state it asks for', async (t) => {
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    return '{"ok":true}'
  })
  for (const step of [{ verify: 'Bold', on: false }, { verify: 'Bold', enabled: false }]) {
    const r = await main({ steps: [step] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate?.reason ?? '', /no element matches "Bold"/)
  }
})

test('a verify by value passes only on the value itself, and a spin button may add a unit to its number', async (t) => {
  let role = 'textbox'; let value = '15'
  const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role, name: 'Size', states: ['showing'], bounds: [10, 10, 100, 20], value })
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root, field()] })
    if (tool === 'desk_read') return JSON.stringify(field())
    return '{"ok":true}'
  })
  const ok = async (want: string) => (await main({ steps: [{ verify: 'Size', value: want }] })).ok
  assert.equal(await ok('5'), false)
  assert.equal(await ok(''), false)
  assert.equal(await ok('15'), true)
  role = 'spinbutton'; value = '12 pt'
  assert.equal(await ok('12'), true)
  assert.equal(await ok('1'), false)
  value = '5.5'
  assert.equal(await ok('5'), false)
})

test('a verify by value on a link compares the url that a read gives it exactly', async (t) => {
  let url = 'https://example.com/'
  const link = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'link', name: 'Docs', states: ['showing'], bounds: [10, 10, 100, 20] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root, link] })
    if (tool === 'desk_read') return JSON.stringify({ ...link, url })
    return '{"ok":true}'
  })
  const ok = async (want: string) => (await main({ steps: [{ verify: 'Docs', role: 'link', value: want }] })).ok
  assert.equal(await ok('https://example.com/'), true)
  assert.equal(await ok('https://example.com/other'), false)
  url = 'https://example.com/a_b'
  assert.equal(await ok('https://example.com/ab'), false)
  assert.equal(await ok('https://example.com/A_b'), false)
  assert.equal(await ok('https://example.com/a_b'), true)
})

test('an expect the tree does not meet is checked on a screenshot, which fails it with the reason or passes it', async (t) => {
  let shows = false
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: null, id: null }, usage })
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(800, 600).toString('base64') }
    if (tool === 'model_ask') return JSON.stringify({ text: JSON.stringify({ ok: shows, why: shows ? 'the text is bold' : 'no text is bold' }), usage })
    return '{"ok":true}'
  })
  const failed = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(failed.ok, false, JSON.stringify(failed))
  assert.match(failed.escalate?.reason ?? '', /^expectation not met: the text is bold \(no text is bold\)/)
  shows = true
  const passed = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(passed.ok, true, JSON.stringify(passed))
  assert.deepEqual(['desk_shot', 'model_ask'].map((c) => calls.filter((x) => x === c).length), [2, 2])
})

test('a screenshot check whose answer is no true or false fails the expect', async (t) => {
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: null, id: null }, usage })
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(800, 600).toString('base64') }
    if (tool === 'model_ask') return JSON.stringify({ text: '{"ok": "false", "why": "no text is bold"}', usage })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /no true or false/)
})

test('a verify that names a role reads an element of that role outside the controls, such as a document body, and one without a role still does not', async (t) => {
  const body = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'document', name: 'Body', states: ['showing'], bounds: [10, 40, 400, 300] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root, body] })
    if (tool === 'desk_read') return JSON.stringify({ ...body, value: 'hello' })
    return '{"ok":true}'
  })
  const ok = async (want: string) => (await main({ steps: [{ verify: 'Body', role: 'document', value: want }] })).ok
  assert.equal(await ok('hello'), true)
  assert.equal(await ok('bye'), false)
  const unnamed = await main({ steps: [{ verify: 'Body', value: 'hello' }] })
  assert.match(unnamed.escalate?.reason ?? '', /no element matches "Body"/)
})
