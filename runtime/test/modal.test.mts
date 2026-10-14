import assert from 'node:assert/strict'
import { mkdtempSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { test } from 'node:test'

test('a step in a window whose app holds a modal dialog the screen does not show escalates with its title and types nothing', async () => {
  const wins = [
    { id: '1.13/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 833, active: true },
    { id: '1.13/339', title: 'Save As', role: 'dialog', pid: 833, active: false, modal: 'hidden', blocks: 'Untitled 1 - Mousepad' },
    { id: '1.18/40', title: 'Terminal - user@box: ~', role: 'window', pid: 1555, active: false },
  ]
  const calls: string[] = []
  const toolhost = createServer((s) => {
    let buf = Buffer.alloc(0)
    s.on('data', (d) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      const { tool } = JSON.parse(buf.subarray(4).toString())
      calls.push(tool)
      let text = '{"ok":true}'
      if (tool === 'desk_windows') text = JSON.stringify(wins)
      if (tool === 'desk_snapshot') text = JSON.stringify({ window: wins[0], nodes: [], truncated: false, text: '- window "Untitled 1 - Mousepad"' })
      const body = Buffer.from(JSON.stringify({ text, is_error: false }))
      const head = Buffer.alloc(4)
      head.writeUInt32BE(body.length)
      s.end(Buffer.concat([head, body]))
    })
  })
  const socket = `${mkdtempSync(`${tmpdir()}/gui-plan-`)}/toolhost.sock`
  await new Promise<void>((r) => toolhost.listen(socket, r))
  process.env.TOOLHOST_SOCKET = socket
  const { main } = await import('../rootfs/opt/desk/lib/gui_plan.mts')
  try {
    const blocked = await main({ steps: [{ type: 'hello' }] })
    assert.equal(blocked.ok, false, JSON.stringify(blocked))
    assert.equal(blocked.escalate.reason, 'modal dialog "Save As" is open but hidden, and "Untitled 1 - Mousepad" takes no input until it closes: finish it, or close it with {close: true, window: "Save As"}')
    assert.ok(blocked.escalate.windows.includes('Save As (modal dialog, hidden)'), JSON.stringify(blocked.escalate.windows))
    assert.ok(!calls.includes('desk_type'), calls.join(' '))
    wins[0].active = false
    wins[1].active = true
    const itself = await main({ steps: [{ type: 'hello' }] })
    assert.equal(itself.escalate?.reason, blocked.escalate.reason, JSON.stringify(itself))
    wins[0].active = true
    wins[1].active = false
    const closing = await main({ steps: [{ close: true, window: 'Save As' }] })
    assert.ok(calls.includes('desk_key'), JSON.stringify(closing))
    wins[0].active = false
    wins[2].active = true
    const other = await main({ steps: [{ type: 'hello' }] })
    assert.equal(other.ok, true, JSON.stringify(other))
    assert.ok(calls.includes('desk_type'), calls.join(' '))
  } finally {
    toolhost.close()
  }
})
