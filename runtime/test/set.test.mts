import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('set passes only when the field reads the value it typed, and an empty value clears the field', async () => {
  const win = { id: '1', title: 'Contact', role: 'dialog', pid: process.pid, active: true }
  let value = 'Bob'; let selected = false; let selects = false
  const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Name', states: ['showing'], bounds: [10, 10, 100, 20], value })
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'dialog', name: 'Contact', states: ['showing'] }, field()] })
    if (tool === 'desk_read') return JSON.stringify(field())
    if (tool === 'desk_key') {
      const k = String(args.keys)
      if (selects && /^(cmd\+a|ctrl\+a|shift\+end)$/.test(k)) selected = true
      if (k === 'delete' && selected) { value = ''; selected = false }
    }
    if (tool === 'desk_type') { value = selected ? String(args.text) : value + String(args.text); selected = false }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const appended = await main({ steps: [{ set: 'Name', value: 'Alice' }] })
    assert.equal(appended.ok, false, JSON.stringify(appended))
    assert.match(appended.escalate?.reason ?? '', /reads "BobAlice" after set/)
    selects = true
    const replaced = await main({ steps: [{ set: 'Name', value: 'Alice' }] })
    assert.equal(replaced.ok, true, JSON.stringify(replaced))
    assert.equal(value, 'Alice')
    const cleared = await main({ steps: [{ set: 'Name', value: '' }] })
    assert.equal(cleared.ok, true, JSON.stringify(cleared))
    assert.equal(value, '')
  } finally {
    server.close()
  }
})
