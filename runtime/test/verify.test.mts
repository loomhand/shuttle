import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Format', role: 'dialog', pid: process.pid, active: true }
const root = { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'dialog', name: 'Format', states: ['showing'] }

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
