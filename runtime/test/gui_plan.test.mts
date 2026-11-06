import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { test } from 'node:test'

import { daemon, notepad, user } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('an app step focuses the app\'s open window, by its process or the window list, instead of launching the app, and names an app that does not exist', { skip: process.platform !== 'linux' }, async (t) => {
  const app = spawn('sleep', ['30'])
  const geo = spawn('sleep', ['30'], { argv0: 'geogebra' })
  t.after(() => { app.kill(); geo.kill() })
  const wins = [
    { id: '1', title: 'Terminal - user@box: ~', role: 'frame', pid: app.pid, active: false },
    { id: '2', title: 'notes.txt - Mousepad', role: 'frame', pid: process.pid, active: true },
  ]
  const listed = [{ title: 'GeoGebra Classic 5', pid: geo.pid }]
  const calls: string[] = []
  await daemon(t, ({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([...wins].sort((a, b) => Number(b.active) - Number(a.active)))
    if (tool === 'desk_snapshot') return JSON.stringify({ window: wins.find((w) => w.active), nodes: [], truncated: false })
    if (tool === 'desk_focus') {
      const want = String(args.window).toLowerCase()
      const all = [...wins, ...listed].filter((x) => args.pid === undefined || x.pid === args.pid)
      const w = all.find((x) => x.title.toLowerCase().includes(want))
      for (const x of wins) x.active = x === w
      return { text: JSON.stringify({ ok: true, title: w?.title, pid: w?.pid }), error: !w }
    }
    if (tool === 'desk_launch') return { text: `no app named "${String(args.app)}"`, error: true }
    return '{"ok":true}'
  })
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

test('a type step sends its whole text in one desk type call, so a closing tag on its last line meets its opener', async (t) => {
  const close = `${String.fromCharCode(60)}/text>`
  const text = `<text x="1">\nhello\n${close}`
  const ws = [{ ...user(), active: false }, { ...notepad(), active: true }]
  const typed: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(ws)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes: [], truncated: false })
    if (tool === 'desk_type') typed.push(args.text)
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ type: text }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(typed, [text])
})
