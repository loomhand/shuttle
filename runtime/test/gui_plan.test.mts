import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

test('an app step focuses the app\'s open window, by its process or the window list, instead of launching the app, and names an app that does not exist', { skip: process.platform !== 'linux' }, async (t) => {
  const app = spawn('sleep', ['30'])
  t.after(() => app.kill())
  const wins = [
    { id: '1', title: 'Terminal - user@box: ~', role: 'frame', pid: app.pid, active: false },
    { id: '2', title: 'notes.txt - Mousepad', role: 'frame', pid: process.pid, active: true },
  ]
  const listed = ['GeoGebra Classic 5']
  const calls: string[] = []
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([...wins].sort((a, b) => Number(b.active) - Number(a.active)))
    if (tool === 'desk_snapshot') return JSON.stringify({ window: wins.find((w) => w.active), nodes: [], truncated: false })
    if (tool === 'desk_focus') {
      const want = String(args.window).toLowerCase()
      const w = wins.find((x) => x.title.toLowerCase().includes(want))
      const title = w?.title ?? listed.find((t) => t.toLowerCase().includes(want))
      for (const x of wins) x.active = x === w
      return { text: JSON.stringify({ ok: true, title, pid: 7 }), error: !title }
    }
    if (tool === 'desk_launch') return { text: `no app named "${String(args.app)}"`, error: true }
    return '{"ok":true}'
  })
  t.after(() => server.close())
  const own = await main({ steps: [{ app: 'sleep' }] })
  assert.equal(own.ok, true, JSON.stringify(own))
  assert.equal(own.window, 'Terminal - user@box: ~')
  const java = await main({ steps: [{ app: 'GeoGebra' }] })
  assert.equal(java.ok, true, JSON.stringify(java))
  assert.ok(!calls.includes('desk_launch'), calls.join(' '))
  const started = Date.now()
  const missing = await main({ steps: [{ app: 'no-such-app-osw' }] })
  assert.equal(missing.ok, false, JSON.stringify(missing))
  assert.equal(missing.escalate?.reason, 'no app named "no-such-app-osw"')
  assert.ok(Date.now() - started < 2000, `${Date.now() - started} ms`)
})
