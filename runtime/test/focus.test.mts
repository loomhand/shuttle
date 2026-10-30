import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

type Field = { role: string; name: string } | null

async function page(t: { after(fn: () => void): void }, front: { title: string; role: string }, focused: Field) {
  const typed: string[] = []
  const win = { id: '1/1', title: front.title, role: front.role, pid: process.pid, active: true, app: 'Browser' }
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') {
      const nodes = [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: front.role, name: front.title, states: ['showing', 'focused'] }]
      if (focused) nodes.push({ ref: 1, id: '1/2', parent: 0, depth: 1, role: focused.role, name: focused.name, states: ['showing', 'focused'] })
      return JSON.stringify({ window: win, truncated: false, nodes })
    }
    if (tool === 'desk_type') typed.push(String(args.text))
    return '{"ok":true}'
  })
  return typed
}

const user = { role: 'textbox', name: '', frame: 'window' }

test('a recorded type step types where the recording typed', async (t) => {
  const typed = await page(t, { title: 'pw', role: 'window' }, { role: 'textbox', name: '' })
  const r = await main({ steps: [{ type: 'bob', focus: user }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(typed, ['bob'])
})

test('a recorded type step stops as an error when a sheet holds the focus, and types nothing', async (t) => {
  const typed = await page(t, { title: 'Save Password?', role: 'dialog' }, { role: 'textbox', name: 'User Name' })
  const r = await main({ steps: [{ type: 'bob', focus: user }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(String(r.error), /the recording typed into textbox "" in a window, and the focus is on textbox "User Name" in the dialog "Save Password\?"/)
  assert.deepEqual(typed, [])
})

test('a recorded type step stops as an error when no focused element can be read', async (t) => {
  const typed = await page(t, { title: 'pw', role: 'window' }, null)
  const r = await main({ steps: [{ type: 'bob', focus: user }] })
  assert.match(String(r.error), /no focused element can be read now/)
  assert.deepEqual(typed, [])
})

test('a type step whose recording read no focus either types without the check', async (t) => {
  const typed = await page(t, { title: 'canvas', role: 'window' }, null)
  const r = await main({ steps: [{ type: 'bob', focus: 'unverified' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(typed, ['bob'])
})
