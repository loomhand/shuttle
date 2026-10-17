import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

const root = (role: string, name: string) => ({ ref: 0, id: '1/1', parent: -1, depth: 0, role, name, states: ['showing'] })

test('a plan with a malformed step or an unfilled param makes no desk call', async () => {
  const calls: string[] = []
  const server = await toolhost(({ tool }) => { calls.push(tool); return '[]' })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const two = await main({ steps: [{ click: 'OK', key: 'Return' }] })
    assert.match(two.escalate?.reason ?? '', /needs exactly one op/)
    const gap = await main({ steps: [{ type: '{{name}}' }] })
    assert.match(gap.escalate?.reason ?? '', /no value for \{\{name\}\}/)
    assert.deepEqual(calls, [])
  } finally {
    server.close()
  }
})

test('a check that does not flip traces the state it found, not the one it wanted', async () => {
  const win = { id: '1', title: 'Preferences', role: 'dialog', pid: process.pid, active: true }
  const box = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'checkbox', name: 'Wrap lines', states: ['showing'], bounds: [10, 10, 20, 20] }
  const server = await toolhost(({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root('dialog', 'Preferences'), box] })
    if (tool === 'desk_read') return JSON.stringify(box)
    if (tool === 'desk_press') return '{"ok":true,"how":"click"}'
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ check: 'Wrap lines', on: true }] })
    assert.match(r.escalate?.reason ?? '', /is still off/)
    assert.deepEqual(r.trace?.[0] && { was: r.trace[0].was, now: r.trace[0].now }, { was: 'off', now: undefined })
  } finally {
    server.close()
  }
})

test('close on a window that answers no accessibility call escalates even when its app has no other window', async () => {
  const win = { id: '1', title: 'Exporting', role: 'dialog', pid: process.pid, active: true }
  const calls: string[] = []
  const server = await toolhost(({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return { text: 'no accessibility answer within 10000 ms; the app may hang', error: true }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ close: true }] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate?.reason ?? '', /answers no accessibility calls/)
    assert.equal(r.escalate?.step, 0, JSON.stringify(r))
    assert.ok(!r.trace?.[0]?.noop, JSON.stringify(r.trace))
    assert.ok(!calls.includes('desk_key'), calls.join(' '))
  } finally {
    server.close()
  }
})

test('typing a Windows path into a dialog field finds it in the outline, backslashes and all', async () => {
  const doc = { id: '1', title: 'a.txt - Notepad', role: 'window', pid: process.pid, active: false }
  const dialog = { id: '2', title: 'Open', role: 'dialog', pid: process.pid, active: true }
  let value = ''
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([doc, dialog])
    if (tool === 'desk_snapshot') {
      const field = { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'textbox', name: 'File name:', states: ['showing', 'focused'], bounds: [10, 10, 100, 20], value }
      return JSON.stringify({ window: dialog, truncated: false, nodes: [root('dialog', 'Open'), field] })
    }
    if (tool === 'desk_type') value += String(args.text)
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ type: 'C:\\Users\\me\\a.txt' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.equal(value, 'C:\\Users\\me\\a.txt')
  } finally {
    server.close()
  }
})
