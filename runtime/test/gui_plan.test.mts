import assert from 'node:assert/strict'
import { spawn } from 'node:child_process'
import { test } from 'node:test'

import { daemon, notepad, root, user } from './daemon.mts'
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

test('the first step takes the start window list only within 25 ms of its read, however long the start outline took', async (t) => {
  const popup = { id: '9', title: '', role: 'dialog', pid: 4, active: true }
  const calls: string[] = []
  const real = Date.now; let now = real()
  Date.now = () => now
  t.after(() => { Date.now = real })
  await daemon(t, ({ tool, args }) => {
    if (args.waiting) now += 10
    else calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([{ ...user(), active: false }, popup])
    if (tool === 'desk_snapshot') {
      if (calls.filter((c) => c === tool).length === 1) now += 40
      return JSON.stringify({ window: popup, nodes: [root('dialog', '')], truncated: false })
    }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ key: 'a' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(calls.slice(0, calls.indexOf('desk_key')), ['desk_windows', 'desk_snapshot', 'desk_snapshot', 'desk_windows'])
})

test('an untitled start window that is no alert is read once for the start check and the first step', async (t) => {
  const popup = { id: '9', title: '', role: 'dialog', pid: 4, active: true }
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([{ ...user(), active: false }, popup])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: popup, nodes: [root('dialog', ''), { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'listitem', name: 'word', states: ['showing'] }], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ key: 'shift+a' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(calls.slice(0, calls.indexOf('desk_key')), ['desk_windows', 'desk_snapshot'])
})

test('an untitled popup a step read before it acts is not read again to leave the step in the plan\'s window', async (t) => {
  const editor = { id: '1', title: 'Editor', role: 'window', pid: 5 }
  const popup = { id: '2', title: '', role: 'dialog', pid: 5 }
  const reads: string[] = []
  let open = false
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...editor, active: !open }, ...(open ? [{ ...popup, active: true }] : [])])
    if (tool === 'desk_snapshot') {
      reads.push(String(args.window))
      if (args.window === '') return JSON.stringify({ window: popup, nodes: [root('window', ''), { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'listitem', name: 'word', states: ['showing'] }], truncated: false })
      return JSON.stringify({ window: editor, nodes: [root('window', 'Editor'), { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'Bold', states: ['showing'] }], truncated: false })
    }
    if (tool === 'desk_press') { reads.push('press'); open = true; return '{"ok":true,"how":"click"}' }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'Bold' }, { click: 'Bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(reads.slice(0, reads.lastIndexOf('press') + 1), ['Editor', 'press', '', 'Editor', 'press'])
})
