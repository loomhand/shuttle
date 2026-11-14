import assert from 'node:assert/strict'
import { mkdirSync, writeFileSync } from 'node:fs'
import { test } from 'node:test'

import { daemon, root } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('a plan with a malformed step or an unfilled param makes no desk call', async (t) => {
  const calls: string[] = []
  await daemon(t, ({ tool }) => { calls.push(tool); return '[]' })
  const two = await main({ steps: [{ click: 'OK', key: 'Return' }] })
  assert.match(two.escalate?.reason ?? '', /needs exactly one op/)
  const gap = await main({ steps: [{ type: '{{name}}' }] })
  assert.match(gap.escalate?.reason ?? '', /no value for \{\{name\}\}/)
  assert.deepEqual(calls, [])
})

test('a check that does not flip traces the state it found, not the one it wanted', async (t) => {
  const win = { id: '1', title: 'Preferences', role: 'dialog', pid: process.pid, active: true }
  const box = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'checkbox', name: 'Wrap lines', states: ['showing'], bounds: [10, 10, 20, 20] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root('dialog', 'Preferences'), box] })
    if (tool === 'desk_read') return JSON.stringify(box)
    if (tool === 'desk_press') return '{"ok":true,"how":"click"}'
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ check: 'Wrap lines', on: true }] })
  assert.match(r.escalate?.reason ?? '', /is still off/)
  assert.deepEqual(r.trace?.[0] && { was: r.trace[0].was, now: r.trace[0].now }, { was: 'off', now: undefined })
})

test('a check on a tri-state box acts again past the mixed state to the state it wants', async (t) => {
  const win = { id: '1', title: 'Properties', role: 'dialog', pid: process.pid, active: true }
  const cycle = [['checkable'], ['checkable', 'checked'], ['checkable', 'mixed']]
  let at = 2
  let presses = 0
  const box = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'checkbox', name: 'Read-only', states: ['showing', ...(cycle[at] ?? [])], bounds: [10, 10, 20, 20] })
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root('dialog', 'Properties'), box()] })
    if (tool === 'desk_read') return JSON.stringify(box())
    if (tool === 'desk_press') { presses++; at = (at + 1) % 3; return '{"ok":true,"how":"click"}' }
    return '{"ok":true}'
  })
  const on = await main({ steps: [{ check: 'Read-only', on: true }] })
  assert.equal(on.ok, true, JSON.stringify(on))
  assert.deepEqual({ at, presses, was: on.trace?.[0]?.was }, { at: 1, presses: 2, was: 'mixed' })
  const off = await main({ steps: [{ check: 'Read-only', on: false }] })
  assert.equal(off.ok, true, JSON.stringify(off))
  assert.deepEqual({ at, presses }, { at: 0, presses: 4 })
})

test('close on a window that answers no accessibility call escalates even when its app has no other window', async (t) => {
  const win = { id: '1', title: 'Exporting', role: 'dialog', pid: process.pid, active: true }
  const calls: string[] = []
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return { text: 'no accessibility answer within 10000 ms; the app may hang', error: true }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ close: true }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /answers no accessibility calls/)
  assert.equal(r.escalate?.step, 0, JSON.stringify(r))
  assert.ok(!r.trace?.[0]?.noop, JSON.stringify(r.trace))
  assert.ok(!calls.includes('desk_key'), calls.join(' '))
})

test('typing a Windows path into a dialog field finds it in the outline, backslashes and all', async (t) => {
  const doc = { id: '1', title: 'a.txt - Notepad', role: 'window', pid: process.pid, active: false }
  const dialog = { id: '2', title: 'Open', role: 'dialog', pid: process.pid, active: true }
  let value = ''
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([doc, dialog])
    if (tool === 'desk_snapshot') {
      const field = { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'textbox', name: 'File name:', states: ['showing', 'focused'], bounds: [10, 10, 100, 20], value }
      return JSON.stringify({ window: dialog, truncated: false, nodes: [root('dialog', 'Open'), field] })
    }
    if (tool === 'desk_type') value += String(args.text)
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ type: 'C:\\Users\\me\\a.txt' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(value, 'C:\\Users\\me\\a.txt')
})

test('a press the desk reports as not done fails the step', async (t) => {
  const win = { id: '1', title: 'Preferences', role: 'dialog', pid: process.pid, active: true }
  const button = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'Apply', states: ['showing'], bounds: [10, 10, 60, 20] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root('dialog', 'Preferences'), button] })
    if (tool === 'desk_press') return '{"ok":false,"how":"action"}'
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'Apply' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /Apply/)
})

test('a step with a non-string value on a text key is refused by key and makes no desk call', async (t) => {
  const calls: string[] = []
  await daemon(t, ({ tool }) => { calls.push(tool); return '[]' })
  for (const [step, key] of [[{ wait: 2 }, 'wait'], [{ type: 5 }, 'type'], [{ type: null }, 'type'], [{ click: ['OK'] }, 'click'], [{ app: 'Notes', window: 3 }, 'window']] as const) {
    const r = await main({ steps: [{ app: 'Notes' }, step as never] })
    assert.equal(r.ok, false, JSON.stringify(step))
    assert.equal(r.done, 0, JSON.stringify(step))
    assert.equal(r.escalate?.step, 1, JSON.stringify(step))
    assert.equal(r.escalate?.reason, `step 1: ${key} takes a string; nothing ran`, JSON.stringify(step))
  }
  mkdirSync(String(process.env.SHUTTLE_FLOWS), { recursive: true })
  writeFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, JSON.stringify({ at: Date.now(), window: null, pid: null, params: {}, steps: [{ app: 'Notes' }, { type: 'x' }, { wait: 'Doc' }], dirty: false, usage: {} }))
  const saved = await main({ steps: [{ app: 'Notes' }, { type: 'x' }, { wait: 2 as never }], save: 'n' })
  assert.equal(saved.escalate?.reason, 'step 2: wait takes a string; nothing ran')
  assert.equal(saved.done, 0)
  assert.deepEqual(calls, [])
})

test('a null on an optional text key is taken as absent', async (t) => {
  await daemon(t, () => '[]')
  const r = await main({ steps: [{ verify: 'Wrap lines', window: null as never }] })
  assert.doesNotMatch(String(r.escalate?.reason), /takes a string|needs exactly one op/)
})

test('a step whose only op is empty is malformed and makes no desk call', async (t) => {
  const calls: string[] = []
  await daemon(t, ({ tool }) => { calls.push(tool); return '[]' })
  for (const step of [{ key: '' }, { click: '' }, { menu: '' }, { wait: '' }, { check: '' }, { close: false }]) {
    const r = await main({ steps: [step] })
    assert.match(r.escalate?.reason ?? '', /needs exactly one op/, JSON.stringify(step))
  }
  assert.deepEqual(calls, [])
})

test('a timeout_ms past the run, or a gone that is no boolean or comes with a state, is refused and makes no desk call', async (t) => {
  const calls: string[] = []
  await daemon(t, ({ tool }) => { calls.push(tool); return '[]' })
  for (const [step, why, timeout_s] of [
    [{ verify: 'Saved', timeout_ms: 0 }, /^step 0: timeout_ms takes a whole number of milliseconds from 1 to 90000,/, undefined],
    [{ verify: 'Saved', timeout_ms: 1.5 }, /^step 0: timeout_ms takes/, undefined],
    [{ verify: 'Saved', timeout_ms: 20000 }, /^step 0: timeout_ms takes a whole number of milliseconds from 1 to 10000,/, 10],
    [{ verify: 'Saved', timeout_ms: 200000 }, /from 1 to 110000,/, 300],
    [{ verify: 'Saved', gone: 'yes' }, /^step 0: gone takes true or false/, undefined],
    [{ verify: 'Saved', gone: true, value: 'x' }, /^step 0: gone takes no value, on or enabled/, undefined],
    [{ verify: 'Saved', gone: true, enabled: false }, /^step 0: gone takes no value, on or enabled/, undefined],
  ] as const) {
    const r = await main({ steps: [step as never], timeout_s })
    assert.equal(r.done, 0, JSON.stringify(step))
    assert.match(r.escalate?.reason ?? '', why, JSON.stringify(step))
  }
  assert.deepEqual(calls, [])
})
