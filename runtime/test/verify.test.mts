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

test('a verify with timeout_ms waits for a label that shows late, and one without it stops at once as before', async (t) => {
  const label = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'text', name: 'Saved', states: ['showing'], bounds: [10, 10, 100, 20] }
  let since = Date.now()
  let snaps = 0
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') { snaps++; return JSON.stringify({ window: win, truncated: false, nodes: Date.now() - since >= 1000 ? [root, label] : [root] }) }
    return '{"ok":true}'
  })
  since = Date.now()
  const waited = await main({ steps: [{ verify: 'Saved', role: 'text', timeout_ms: 3000 }] })
  assert.equal(waited.ok, true, JSON.stringify(waited))
  assert.ok((waited.ms ?? 0) >= 1000 && (waited.ms ?? 0) < 3000, JSON.stringify(waited))
  assert.ok(snaps <= 16, String(snaps))
  since = Date.now()
  const once = await main({ steps: [{ verify: 'Saved', role: 'text' }] })
  assert.equal(once.ok, false, JSON.stringify(once))
  assert.match(once.escalate?.reason ?? '', /^verify: no element matches "Saved"/)
  assert.ok((once.ms ?? 0) < 1000, JSON.stringify(once))
})

test('a verify with gone passes once its element goes, and stops naming it when it stays past timeout_ms', async (t) => {
  const label = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'text', name: 'Saving', states: ['showing'], bounds: [10, 10, 100, 20] }
  let until = Infinity
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: Date.now() < until ? [root, label] : [root] })
    return '{"ok":true}'
  })
  const stays = await main({ steps: [{ verify: 'Saving', role: 'text', gone: true, timeout_ms: 500 }] })
  assert.equal(stays.ok, false, JSON.stringify(stays))
  assert.match(stays.escalate?.reason ?? '', /^verify: "Saving" is still there/)
  assert.ok((stays.ms ?? 0) >= 500, JSON.stringify(stays))
  until = Date.now() + 400
  const goes = await main({ steps: [{ verify: 'Saving', role: 'text', gone: true, timeout_ms: 3000 }] })
  assert.equal(goes.ok, true, JSON.stringify(goes))
})

test('a verify with gone counts a shown element without bounds as still there', async (t) => {
  const button = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'Show', states: ['showing'], bounds: [10, 10, 60, 20] }
  const label = { ref: 2, id: '1/3', parent: 0, depth: 1, role: 'text', name: 'Done', states: ['showing'] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root, button, label] })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ verify: 'Done', gone: true, timeout_ms: 300 }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /^verify: "Done" is still there/)
})

test('a waiting verify asks no model until its last read, and one that had to wait confirms the step before it', async (t) => {
  const button = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'Show', states: ['showing'], bounds: [10, 10, 60, 20] }
  const label = { ref: 2, id: '1/3', parent: 0, depth: 1, role: 'text', name: 'Done', states: ['showing'], bounds: [10, 40, 60, 20] }
  let shown = Infinity; let asked = 0
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: Date.now() >= shown ? [root, button, label] : [root, button] })
    if (tool === 'desk_click' || tool === 'desk_press' || tool === 'desk_act') { shown = Date.now() + 600; return JSON.stringify({ ok: true, unverified: 'unverifiable: posted click' }) }
    if (tool.startsWith('model_')) { asked++; throw new Error('model offline') }
    return '{"ok":true}'
  })
  process.env.SHUTTLE_INPUT = 'background'
  const mode = 'verify-bg'
  const background = ((await import(`../lib/gui_plan.mts?${mode}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_INPUT
  const r = await background({ steps: [{ wait: 'Format' }, { click: 'Show' }, { verify: 'Done', role: 'text', timeout_ms: 3000 }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(asked, 0)
})
