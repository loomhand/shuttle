import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('an app step finds a running app by the app its windows name, whatever their titles', async (t) => {
  const doc = { id: '1', title: 'Untitled', role: 'window', pid: process.pid, active: false, app: 'TextEdit' }
  const calls: string[] = []
  await daemon(t, ({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([doc])
    if (tool === 'desk_focus') {
      if (args.window !== doc.title) return { text: `no window "${String(args.window)}"`, error: true }
      doc.active = true
      return JSON.stringify({ ok: true, title: doc.title, pid: doc.pid })
    }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'TextEdit' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, 'Untitled')
  assert.ok(!calls.includes('desk_launch'), calls.join(' '))
})

test('an app step takes the window a launch raised, whatever its app and title are called', async (t) => {
  const term = { id: '1', title: 'Terminal - user@box: ~', role: 'window', pid: process.pid, active: false, app: 'xfce4-terminal' }
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([term])
    if (tool === 'desk_launch') { term.active = true; return JSON.stringify({ ok: true, launched: 'raised', running: true, title: term.title, pid: term.pid }) }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: term, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'Xfce Terminal' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, term.title)
  assert.equal(calls.filter((c) => c === 'desk_launch').length, 1, calls.join(' '))
})

test('an app step for a package takes the window that ends in its package, not one whose package it prefixes', async (t) => {
  const suggestions = { id: '7', title: 'Settings Suggestions - com.android.settings.intelligence', role: 'frame', pid: 4562183, active: true }
  const settings = { id: '9', title: 'Settings - com.android.settings', role: 'frame', pid: 6289366, active: false }
  const calls: string[] = []
  await daemon(t, ({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([suggestions, settings])
    if (tool === 'desk_focus') {
      if (args.window !== settings.title) return { text: `no window "${String(args.window)}"`, error: true }
      settings.active = true; suggestions.active = false
      return JSON.stringify({ ok: true, title: settings.title, pid: settings.pid })
    }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: settings, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'com.android.settings' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, settings.title)
  assert.ok(!calls.includes('desk_launch'), calls.join(' '))
})

test('an app step for a package launches it when only a package it prefixes has a window', async (t) => {
  const suggestions = { id: '7', title: 'Settings Suggestions - com.android.settings.intelligence', role: 'frame', pid: 4562183, active: true }
  const settings = { id: '9', title: 'Settings - com.android.settings', role: 'frame', pid: 6289366, active: true }
  let launched = false
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify(launched ? [{ ...suggestions, active: false }, settings] : [suggestions])
    if (tool === 'desk_focus') return { text: 'no window "com.android.settings"', error: true }
    if (tool === 'desk_launch') { launched = true; return JSON.stringify({ ok: true, launched: 'com.android.settings/.Settings' }) }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: settings, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'com.android.settings' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, settings.title)
  assert.ok(calls.includes('desk_launch'), calls.join(' '))
})

test('an app step for a name that is no package still takes a window whose title contains it', async (t) => {
  const doc = { id: '1', title: 'notes.txt - Notes Editor', role: 'window', pid: process.pid, active: false }
  const calls: string[] = []
  await daemon(t, ({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([doc])
    if (tool === 'desk_focus') {
      if (args.window !== doc.title) return { text: `no window "${String(args.window)}"`, error: true }
      doc.active = true
      return JSON.stringify({ ok: true, title: doc.title, pid: doc.pid })
    }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'Notes Editor' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, doc.title)
  assert.ok(!calls.includes('desk_launch'), calls.join(' '))
})

test('an app step brings a window it launched in the background to the front, even when the window reads active', async (t) => {
  const doc = { id: '1', title: 'Untitled', role: 'window', pid: process.pid, active: true, app: 'TextEdit' }
  let launched = false
  const focused: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(launched ? [doc] : [])
    if (tool === 'desk_focus') {
      if (!launched) return { text: `no window "${String(args.window)}"`, error: true }
      focused.push(args.window)
      return JSON.stringify({ ok: true, title: doc.title, pid: doc.pid })
    }
    if (tool === 'desk_launch') { launched = true; return JSON.stringify({ ok: true, launched: 'Launched TextEdit in background.', background: true }) }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'TextEdit' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(focused, ['Untitled'])
})

test('an app step leaves an active window as it is when the launch stays in front or the app was already open', async (t) => {
  const doc = { id: '1', title: 'Untitled - Notepad', role: 'window', pid: process.pid, active: true, app: 'Notepad' }
  let launched = false
  const focused: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(launched ? [doc] : [])
    if (tool === 'desk_focus') {
      if (launched) focused.push(args.window)
      return launched ? JSON.stringify({ ok: true, title: doc.title, pid: doc.pid }) : { text: `no window "${String(args.window)}"`, error: true }
    }
    if (tool === 'desk_launch') { launched = true; return JSON.stringify({ ok: true, launched: 'notepad' }) }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: doc, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const fresh = await main({ steps: [{ app: 'Notepad' }] })
  assert.equal(fresh.ok, true, JSON.stringify(fresh))
  const open = await main({ steps: [{ app: 'Notepad' }] })
  assert.equal(open.ok, true, JSON.stringify(open))
  assert.deepEqual(focused, [])
})

test('an app step takes the window of the app it names before a window whose title holds the name, and never one that holds it inside another word', async (t) => {
  const gmail = { id: '1', title: 'Gmail - Inbox', role: 'window', pid: 7, active: true, app: 'Google Chrome' }
  const mail = { id: '2', title: 'Inbox', role: 'window', pid: 8, active: false, app: 'Mail' }
  let open = false
  const calls: string[] = []
  await daemon(t, ({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify(open ? [gmail, mail] : [gmail])
    if (tool === 'desk_focus') {
      const want = String(args.window); const lower = want.trim().toLowerCase()
      const all = (open ? [gmail, mail] : [gmail]).filter((w) => args.pid === undefined || w.pid === args.pid)
      const w = all.find((x) => x.title === want) ?? all.find((x) => x.title.trim().toLowerCase() === lower) ?? all.find((x) => x.title.toLowerCase().includes(lower))
      if (!w) return { text: `no window "${want}"`, error: true }
      for (const x of [gmail, mail]) x.active = x === w
      return JSON.stringify({ ok: true, title: w.title, pid: w.pid })
    }
    if (tool === 'desk_launch') { open = true; return JSON.stringify({ ok: true, launched: 'Mail' }) }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: mail, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const closed = await main({ steps: [{ app: 'Mail' }] })
  assert.equal(closed.ok, true, JSON.stringify(closed))
  assert.equal(closed.window, 'Inbox')
  assert.equal(calls.filter((c) => c === 'desk_launch').length, 1, calls.join(' '))
  mail.active = false; gmail.active = true; calls.length = 0
  const running = await main({ steps: [{ app: 'Mail' }] })
  assert.equal(running.ok, true, JSON.stringify(running))
  assert.equal(running.window, 'Inbox')
  assert.ok(!calls.includes('desk_launch'), calls.join(' '))
})

test('an app step after a launch takes a new window of the launched app, never a window another app retitled meanwhile', async (t) => {
  const tab = { id: '1', title: 'Docs - Browser', role: 'window', pid: 7, active: true, app: 'Browser' }
  const mail = { id: '2', title: 'Inbox', role: 'window', pid: 8, active: true, app: 'Mailer' }
  let lists = -1
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') {
      if (lists < 0) return JSON.stringify([tab])
      lists += 1
      return JSON.stringify(lists < 4 ? [{ ...tab, title: 'News - Browser' }] : [{ ...tab, title: 'News - Browser', active: false }, mail])
    }
    if (tool === 'desk_focus') return { text: 'no window', error: true }
    if (tool === 'desk_launch') { lists = 0; return JSON.stringify({ ok: true, launched: 'Mail' }) }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: mail, nodes: [], truncated: false })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'Mail' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.window, 'Inbox')
})

test('an app step escalates at once on a listed window whose app stopped answering, and never launches another instance', async (t) => {
  const calc = { id: '1.62/3', title: 'sales.ods — LibreOffice Calc', role: 'window', pid: 7076, active: true, app: 'soffice.bin', stale: true }
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([calc])
    if (tool === 'desk_launch') return JSON.stringify({ ok: true, launched: 'libreoffice-calc' })
    return { text: 'window list is stale: "sales.ods — LibreOffice Calc" did not answer in time', error: true }
  })
  const r = await main({ steps: [{ app: 'libreoffice calc' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /is not answering accessibility: "sales\.ods — LibreOffice Calc" \(pid 7076\)/)
  assert.deepEqual(r.escalate?.windows, [calc.title])
  assert.ok(!calls.includes('desk_launch'), calls.join(' '))
})

test('an app step escalates instead of launching when the front window of the app answers no window list', async (t) => {
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([{ id: '1.7/1', title: 'Desktop', role: 'window', pid: 781, active: false, app: 'xfdesktop', shell: true }])
    if (tool === 'desk_unanswered') return JSON.stringify({ id: '', title: 'sales.ods — LibreOffice Calc', role: 'window', pid: 6204, active: false, app: 'soffice.bin' })
    if (tool === 'desk_launch') return JSON.stringify({ ok: true, launched: 'libreoffice-calc' })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'LibreOffice Calc' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /is not answering accessibility: "sales\.ods — LibreOffice Calc" \(pid 6204\)/)
  assert.ok(!calls.includes('desk_launch'), calls.join(' '))
})
