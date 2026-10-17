import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

const root = (role: string, name: string) => ({ ref: 0, id: '1/1', parent: -1, depth: 0, role, name, states: ['showing'] })

function desktop() {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const dialog = { id: '2', title: 'Rename', role: 'dialog', pid: process.pid, active: false }
  const calls: string[] = []
  let value = ''
  const answer = ({ tool, args }: { tool: string; args: Record<string, unknown> }) => {
    calls.push(tool)
    const wins = dialog.active ? [doc, dialog] : [doc]
    if (tool === 'desk_windows') return JSON.stringify(wins)
    if (tool === 'desk_snapshot') {
      const w = dialog.active ? dialog : doc
      const field = { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'textbox', name: 'Name', states: ['showing', 'focused'], bounds: [10, 10, 100, 20], value }
      return JSON.stringify({ window: w, truncated: false, nodes: [root(w.role, w.title), field] })
    }
    if (tool === 'desk_key' && args.keys === 'f2') { doc.active = false; dialog.active = true }
    if (tool === 'desk_type') value += String(args.text)
    return '{"ok":true}'
  }
  return { calls, answer }
}

test('a type step into a dialog that opened mid-plan reads the dialog once before typing', async () => {
  const d = desktop()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'f2' }, { type: 'b.txt' }] })
    assert.equal(r.done, 2, JSON.stringify(r))
    const between = d.calls.slice(d.calls.indexOf('desk_key'), d.calls.indexOf('desk_type'))
    assert.equal(between.filter((t) => t === 'desk_snapshot').length, 1, between.join(' '))
  } finally {
    server.close()
  }
})

test('a step in a dialog this plan already read reads no tree when its op needs none', async () => {
  const d = desktop()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'f2' }, { type: 'b' }, { key: 'tab' }] })
    assert.equal(r.done, 3, JSON.stringify(r))
    const tab = d.calls.lastIndexOf('desk_key')
    const step = d.calls.slice(d.calls.lastIndexOf('desk_type') + 1, tab)
    assert.equal(step.filter((t) => t === 'desk_snapshot').length, 1, step.join(' '))
  } finally {
    server.close()
  }
})

test('a plan that ends in a plain window reads no tree to check what it left in front', async () => {
  const d = desktop()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'ctrl+a' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.ok(!d.calls.includes('desk_snapshot'), d.calls.join(' '))
  } finally {
    server.close()
  }
})
