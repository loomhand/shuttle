import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('a step in a window whose app holds a modal dialog the screen does not show escalates with its title and types nothing', async () => {
  const wins = [
    { id: '1.13/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 833, active: true },
    { id: '1.13/339', title: 'Save As', role: 'dialog', pid: 833, active: false, modal: 'hidden', blocks: 'Untitled 1 - Mousepad' },
    { id: '1.18/40', title: 'Terminal - user@box: ~', role: 'window', pid: 1555, active: false },
  ]
  const calls: string[] = []
  const [doc, dialog, terminal] = wins
  assert.ok(doc && dialog && terminal)
  const server = await toolhost(({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify(wins)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false, text: '- window "Untitled 1 - Mousepad"' })
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const blocked = await main({ steps: [{ type: 'hello' }] })
    assert.equal(blocked.ok, false, JSON.stringify(blocked))
    assert.equal(blocked.escalate?.reason, 'modal dialog "Save As" is open but hidden, and "Untitled 1 - Mousepad" takes no input until it closes: finish it, or close it with {close: true, window: "Save As"}')
    assert.ok(blocked.escalate?.windows?.includes('Save As (modal dialog, hidden)'), JSON.stringify(blocked.escalate))
    assert.ok(!calls.includes('desk_type'), calls.join(' '))
    doc.active = false
    dialog.active = true
    const itself = await main({ steps: [{ type: 'hello' }] })
    assert.equal(itself.escalate?.reason, blocked.escalate?.reason, JSON.stringify(itself))
    doc.active = true
    dialog.active = false
    const closing = await main({ steps: [{ close: true, window: 'Save As' }] })
    assert.ok(calls.includes('desk_key'), JSON.stringify(closing))
    doc.active = false
    terminal.active = true
    const other = await main({ steps: [{ type: 'hello' }] })
    assert.equal(other.ok, true, JSON.stringify(other))
    assert.ok(calls.includes('desk_type'), calls.join(' '))
  } finally {
    server.close()
  }
})
