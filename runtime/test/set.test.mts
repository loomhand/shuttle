import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

test('set passes only when the field reads exactly the value it typed, and an empty value presses delete to clear the field', async (t) => {
  const win = { id: '1', title: 'Contact', role: 'dialog', pid: process.pid, active: true }
  let value = 'Bob'; let selected = false; let selects = false; let readsAfterTyping = true; let typed = false
  const keys: string[] = []
  const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Name', states: ['showing'], bounds: [10, 10, 100, 20], value })
  await toolhost(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'dialog', name: 'Contact', states: ['showing'] }, field()] })
    if (tool === 'desk_read') return typed && !readsAfterTyping ? { text: 'no accessibility answer', error: true } : JSON.stringify(field())
    if (tool === 'desk_key') {
      const k = String(args.keys); keys.push(k)
      if (selects && /^(cmd\+a|ctrl\+a|shift\+end)$/.test(k)) selected = true
      if (k === 'delete' && selected) { value = ''; selected = false }
    }
    if (tool === 'desk_type' && String(args.text)) { typed = true; value = selected ? String(args.text) : value + String(args.text); selected = false }
    return '{"ok":true}'
  })
  const appended = await main({ steps: [{ set: 'Name', value: 'Alice' }] })
  assert.match(appended.escalate?.reason ?? '', /reads "BobAlice" after set/, JSON.stringify(appended))
  value = 'Alice'
  const doubled = await main({ steps: [{ set: 'Name', value: 'Alice' }] })
  assert.match(doubled.escalate?.reason ?? '', /reads "AliceAlice" after set/, JSON.stringify(doubled))
  selects = true; readsAfterTyping = false
  const unread = await main({ steps: [{ set: 'Name', value: 'Carol' }] })
  assert.equal(unread.ok, false, JSON.stringify(unread))
  readsAfterTyping = true
  const replaced = await main({ steps: [{ set: 'Name', value: 'Alice' }] })
  assert.equal(replaced.ok, true, JSON.stringify(replaced))
  assert.equal(value, 'Alice')
  keys.length = 0
  const cleared = await main({ steps: [{ set: 'Name', value: '' }] })
  assert.equal(cleared.ok, true, JSON.stringify(cleared))
  assert.ok(keys.includes('delete'), keys.join(' '))
  assert.equal(value, '')
})

test('set sends no key when the field takes no focus, so another field keeps its text', async (t) => {
  const win = { id: '1', title: 'Contact', role: 'dialog', pid: process.pid, active: true }
  const calls: string[] = []
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Name', states: ['showing'], bounds: [10, 10, 100, 20], value: 'Bob' }
  await toolhost(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'dialog', name: 'Contact', states: ['showing'] }, field] })
    if (tool === 'desk_read') return JSON.stringify(field)
    if (tool === 'desk_act') return '{"ok":false}'
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ set: 'Name', value: 'Alice' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.ok(!calls.includes('desk_key') && !calls.includes('desk_type'), calls.join(' '))
})
