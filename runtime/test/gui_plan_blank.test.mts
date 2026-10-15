import assert from 'node:assert/strict'
import { mkdtempSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { test } from 'node:test'

test('a step that typed into an empty field stops when its app was launched and reopened its last document', async () => {
  let launched = false
  const typed: string[] = []
  const win = { id: '1', title: 'alice.txt - Mousepad', role: 'frame', pid: process.pid, active: true }
  const toolhost = createServer((s) => {
    let buf = Buffer.alloc(0)
    s.on('data', (d) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      const { tool, args } = JSON.parse(buf.subarray(4).toString())
      let text = '{"ok":true}'
      let error = false
      if (tool === 'desk_windows') text = JSON.stringify(launched ? [win] : [])
      if (tool === 'desk_focus') { error = !launched; text = error ? 'no such window' : JSON.stringify({ ok: true, title: win.title, pid: win.pid }) }
      if (tool === 'desk_launch') launched = true
      if (tool === 'desk_type') typed.push(args.text)
      if (tool === 'desk_snapshot') {
        text = JSON.stringify({
          window: win, truncated: false,
          nodes: [
            { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'frame', name: win.title, states: ['showing', 'active'] },
            { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused'], value: 'alice lives in Lisbon' },
          ],
        })
      }
      const body = Buffer.from(JSON.stringify({ text, is_error: error }))
      const head = Buffer.alloc(4)
      head.writeUInt32BE(body.length)
      s.end(Buffer.concat([head, body]))
    })
  })
  const socket = `${mkdtempSync(`${tmpdir()}/gui-plan-blank-`)}/toolhost.sock`
  await new Promise<void>((r) => toolhost.listen(socket, r))
  process.env.TOOLHOST_SOCKET = socket
  const { main } = await import('../rootfs/opt/desk/lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ app: 'mousepad' }, { type: 'bob lives in Oslo', blank: { role: 'textbox', name: '', parent: '', nth: 0 } }] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate.reason, /already holds text/)
    assert.deepEqual(typed, [])
  } finally {
    toolhost.close()
  }
})
