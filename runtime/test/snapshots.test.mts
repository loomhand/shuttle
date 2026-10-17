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

test('a plan whose last step cannot open anything ends in a plain window without a tree read', async () => {
  const d = desktop()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ wait: 'a.txt' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.ok(!d.calls.includes('desk_snapshot'), d.calls.join(' '))
  } finally {
    server.close()
  }
})

test('a typed line after a shortcut waits for the file chooser the shortcut opens late, and takes it as a chooser', async () => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const dialog = { id: '2', title: 'Save As', role: 'dialog', pid: process.pid, active: false }
  const calls: string[] = []
  let shown = false
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool === 'desk_key' ? `key ${String(args.keys)}` : tool)
    const wins = shown ? [{ ...doc, active: false }, dialog] : [doc]
    if (tool === 'desk_windows') return JSON.stringify(wins)
    if (tool === 'desk_snapshot') {
      const nodes = shown
        ? [root('dialog', 'Save As'), { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'text', name: 'Save As:', states: ['showing'] }, { ref: 2, id: '2/3', parent: 0, depth: 1, role: 'textbox', name: 'File name:', states: ['showing', 'focused'] }, { ref: 3, id: '2/4', parent: 0, depth: 1, role: 'filler', name: 'File Chooser Widget', states: ['showing'] }]
        : [root('window', doc.title)]
      return JSON.stringify({ window: shown ? dialog : doc, truncated: false, nodes })
    }
    if (tool === 'desk_key' && args.keys === 'ctrl+shift+s') setTimeout(() => { shown = true; dialog.active = true }, 200)
    if (tool === 'desk_key' && /^return$/i.test(String(args.keys))) shown = false
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    await main({ steps: [{ key: 'ctrl+shift+s' }, { type: 'b.txt\n' }] })
    const typed = calls.slice(calls.indexOf('key ctrl+shift+s') + 1, calls.indexOf('desk_type'))
    assert.ok(typed.some((c) => c.startsWith('key ')), `no select key before typing into the chooser: ${typed.join(' ')}`)
  } finally {
    server.close()
  }
})

test('a plan whose last key leaves its app blocked behind a dialog it cannot see escalates instead of passing', async () => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  let blocked = false
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([doc])
    if (tool === 'desk_snapshot') return blocked ? { text: 'no accessibility answer within 3000 ms; the app may hang', error: true } : JSON.stringify({ window: doc, truncated: false, nodes: [root('window', doc.title)] })
    if (tool === 'desk_key' && args.keys === 'ctrl+e') blocked = true
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'ctrl+e' }] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate?.reason ?? '', /answers no accessibility calls/)
  } finally {
    server.close()
  }
})
