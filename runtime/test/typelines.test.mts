import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

async function plan(t: TestContext, twin = false, untitled = false) {
  const typed: unknown[] = []
  const win = { id: '1/1', title: 'Doc', role: 'window', pid: process.pid, active: !untitled, app: 'Editor' }
  const ws = twin ? [win, { ...win, id: '1/2', active: false }] : untitled ? [{ ...win, id: '1/3', title: '', active: true }, win] : [win]
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(ws)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Doc', states: ['showing'] }] })
    if (tool === 'desk_type') typed.push(args)
    return '{"ok":true}'
  })
  return typed
}

test('a foreground type with a line after a Return names its window, so the desk checks it after the Return', async (t) => {
  const typed = await plan(t)
  const r = await main({ steps: [{ type: 'a\nb' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(typed, [{ text: 'a\nb', window: 'Doc', pid: process.pid }])
})

test('a foreground type whose only Return ends the text names no window', async (t) => {
  const typed = await plan(t)
  const r = await main({ steps: [{ type: 'a' }, { type: 'report\n' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(typed, [{ text: 'a' }, { text: 'report\n' }])
})

test('a foreground multi-line type names no window when another window of the app has its title', async (t) => {
  const typed = await plan(t, true)
  const r = await main({ steps: [{ type: 'a\nb' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(typed, [{ text: 'a\nb' }])
})

test('a foreground multi-line type into an untitled focused window names none of its app\'s titled windows', async (t) => {
  const typed = await plan(t, false, true)
  const r = await main({ steps: [{ type: 'a\nb' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(typed, [{ text: 'a\nb' }])
})
