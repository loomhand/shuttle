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
