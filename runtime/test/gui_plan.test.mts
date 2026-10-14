import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { mkdtempSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { test } from 'node:test'

test('an app step focuses the app\'s open window, by its process or the window list, instead of launching the app', async () => {
  const app = spawn('sleep', ['30'])
  const wins = [
    { id: '1', title: 'Terminal - user@box: ~', role: 'frame', pid: app.pid, active: false },
    { id: '2', title: 'notes.txt - Mousepad', role: 'frame', pid: process.pid, active: true },
  ]
  const listed = ['GeoGebra Classic 5']
  const calls: string[] = []
  const toolhost = createServer((s) => {
    let buf = Buffer.alloc(0)
    s.on('data', (d) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      const { tool, args } = JSON.parse(buf.subarray(4).toString())
      calls.push(tool)
      let text = '{"ok":true}'
      let error = tool === 'desk_launch'
      if (tool === 'desk_windows') text = JSON.stringify([...wins].sort((a, b) => Number(b.active) - Number(a.active)))
      if (tool === 'desk_snapshot') text = JSON.stringify({ window: wins.find((w) => w.active), nodes: [], truncated: false })
      if (tool === 'desk_focus') {
        const want = String(args.window).toLowerCase()
        const w = wins.find((x) => x.title.toLowerCase().includes(want))
        const title = w?.title ?? listed.find((t) => t.toLowerCase().includes(want))
        for (const x of wins) x.active = x === w
        text = JSON.stringify({ ok: true, title, pid: 7 })
        error = !title
      }
      const body = Buffer.from(JSON.stringify({ text, is_error: error }))
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
    const own = await main({ steps: [{ app: 'sleep' }] })
    assert.equal(own.ok, true, JSON.stringify(own))
    assert.equal(own.window, 'Terminal - user@box: ~')
    const java = await main({ steps: [{ app: 'GeoGebra' }] })
    assert.equal(java.ok, true, JSON.stringify(java))
    assert.ok(!calls.includes('desk_launch'), calls.join(' '))
  } finally {
    app.kill()
    toolhost.close()
  }
})
