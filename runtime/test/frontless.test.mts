import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

async function unfocused(t: TestContext, titles: string[], role = 'dialog', busy: () => object | null = () => null) {
  const wins = [{ id: '1', title: 'StreamView - Google Chrome', role: 'frame', pid: process.pid, active: false }, ...titles.map((title, i) => ({ id: String(i + 2), title, role, pid: process.pid, active: false }))]
  const calls: string[] = []
  await daemon(t, ({ tool, args }) => {
    calls.push(tool === 'desk_focus' || tool === 'desk_press' ? `${tool} ${String(args.window ?? args.id)}` : tool)
    if (tool === 'desk_windows') return JSON.stringify(wins)
    if (tool === 'desk_focus') return JSON.stringify({ ok: true, title: args.window, pid: process.pid })
    if (tool === 'desk_snapshot') {
      const w = wins.find((x) => x.title === args.window) ?? wins[0]
      return JSON.stringify({ window: w, truncated: false, nodes: [{ ref: 0, id: `${w?.id}/1`, parent: -1, depth: 0, role: 'dialog', name: w?.title, states: ['showing'] }, { ref: 1, id: `${w?.id}/2`, parent: 0, depth: 1, role: 'button', name: 'Cancel', states: ['showing'], bounds: [10, 10, 60, 20] }] })
    }
    if (tool === 'desk_press') return '{"ok":true,"how":"action"}'
    if (tool === 'desk_unanswered') return JSON.stringify(busy())
    return '{"ok":true}'
  })
  return calls
}

test('with no window in front, a wait step takes and focuses the one listed window of its title, and the next step acts in it', async (t) => {
  const calls = await unfocused(t, ['Open File'])
  const r = await main({ steps: [{ wait: 'Open File', timeout_ms: 1000 }, { click: 'Cancel' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(calls.filter((c) => c.startsWith('desk_focus') || c.startsWith('desk_press')), ['desk_focus Open File', 'desk_focus Open File', 'desk_press 2/2'])
})

test('with no window in front, a wait step does not choose between two windows of its title, and a step without one plan window stops', async (t) => {
  const calls = await unfocused(t, ['Open File', 'Open File'])
  const waited = await main({ steps: [{ wait: 'Open File', timeout_ms: 600 }] })
  assert.equal(waited.escalate?.reason, 'expected window "Open File" did not appear', JSON.stringify(waited))
  const clicked = await main({ steps: [{ click: 'Cancel' }] })
  assert.equal(clicked.escalate?.reason, 'no focused window', JSON.stringify(clicked))
  assert.ok(!calls.some((c) => c.startsWith('desk_focus') || c.startsWith('desk_press')), calls.join(' '))
})

test('a plan does not take the last window of the plan before it when no window is in front', async (t) => {
  const calls = await unfocused(t, ['Open File'])
  const first = await main({ steps: [{ wait: 'Open File', timeout_ms: 1000 }] })
  assert.equal(first.ok, true, JSON.stringify(first))
  const before = calls.length
  const next = await main({ steps: [{ click: 'Cancel' }] })
  assert.equal(next.escalate?.reason, 'no focused window', JSON.stringify(next))
  assert.ok(!calls.slice(before).some((c) => c.startsWith('desk_focus') || c.startsWith('desk_press')), calls.slice(before).join(' '))
})

test('with no window in front, a wait step never takes an alert that carries its title', async (t) => {
  const calls = await unfocused(t, ['Confirm Save As'], 'alert')
  const r = await main({ steps: [{ wait: 'Save As', timeout_ms: 600 }, { click: 'Cancel' }] })
  assert.equal(r.escalate?.reason, 'expected window "Save As" did not appear', JSON.stringify(r))
  assert.ok(!calls.some((c) => c.startsWith('desk_focus') || c.startsWith('desk_press')), calls.join(' '))
})

test('a plan waits while the front app does not answer accessibility, and names it with the time it waited', async (t) => {
  const calc = { id: '', title: 'answers.xlsx - LibreOffice Calc', role: 'window', pid: 5173, active: false, app: 'soffice.bin' }
  let until = Infinity
  await unfocused(t, [], 'dialog', () => Date.now() < until ? calc : null)
  const started = Date.now()
  const stuck = await main({ steps: [{ click: 'Save' }] })
  assert.match(stuck.escalate?.reason ?? '', /^front app "soffice\.bin" \(pid 5173\) is not answering accessibility; waited 2\d\.\d s for the front app to answer accessibility$/, JSON.stringify(stuck))
  assert.ok(Date.now() - started < 23000, `${Date.now() - started} ms`)
  until = Date.now() + 300
  const answered = await main({ steps: [{ click: 'Save' }] })
  assert.match(answered.escalate?.reason ?? '', /^no focused window; waited 0\.\d s for the front app to answer accessibility$/, JSON.stringify(answered))
  until = Date.now() + 700
  const waited = await main({ steps: [{ wait: 'Keep current format', timeout_ms: 400 }] })
  assert.match(waited.escalate?.reason ?? '', /^expected window "Keep current format" did not appear; waited 0\.\d s for the front app to answer accessibility$/, JSON.stringify(waited))
})

test('a step whose front app does not answer accessibility runs again once it answers', async (t) => {
  const dialog = { id: '2', title: 'Keep current format', role: 'dialog', pid: process.pid, active: false }
  const calc = { id: '', title: 'answers.xlsx - LibreOffice Calc', role: 'window', pid: 5173, active: false, app: 'soffice.bin' }
  const until = Date.now() + 1000
  const calls: string[] = []
  let kept = false
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    const busy = Date.now() < until
    if (tool === 'desk_windows') return JSON.stringify(busy ? [] : kept ? [{ ...calc, id: '1', role: 'frame', active: true }] : [{ ...dialog, active: true }])
    if (tool === 'desk_unanswered') return JSON.stringify(busy ? calc : null)
    if (tool === 'desk_snapshot' && kept) return JSON.stringify({ window: calc, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: calc.title, states: ['showing'] }] })
    if (tool === 'desk_snapshot') return JSON.stringify({ window: dialog, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: dialog.title, states: ['showing'] }, { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'button', name: 'Use Excel 2007-365!', states: ['showing'], bounds: [10, 10, 60, 20] }] })
    if (tool === 'desk_press') { kept = true; return '{"ok":true,"how":"action"}' }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'Use Excel 2007-365!' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.ok(calls.includes('desk_press'), calls.join(' '))
  assert.ok(calls.filter((c) => c === 'desk_unanswered').length >= 2, calls.join(' '))
  assert.ok((r.trace?.[0]?.busy ?? 0) >= 500, JSON.stringify(r.trace))
})

test('a step that finds no window in front only until the window the key opened shows recovers without asking for an unanswered app', async (t) => {
  const dialog = { id: '2', title: 'Open File', role: 'dialog', pid: process.pid, active: false }
  const calls: string[] = []
  let keyed = 0
  await daemon(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') { if (keyed && Date.now() - keyed > 1000) dialog.active = true; return JSON.stringify(keyed ? [dialog] : [{ ...dialog, id: '1', title: 'a.txt - Editor', role: 'frame', active: true }]) }
    if (tool === 'desk_key') { keyed = Date.now(); return '{"ok":true}' }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: dialog, truncated: false, nodes: [{ ref: 0, id: '2/1', parent: -1, depth: 0, role: 'dialog', name: 'Open File', states: ['showing'] }, { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'button', name: 'Cancel', states: ['showing'], bounds: [10, 10, 60, 20] }] })
    if (tool === 'desk_press') return '{"ok":true,"how":"action"}'
    if (tool === 'desk_unanswered') return 'null'
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ key: 'ctrl+o' }, { click: 'Cancel' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.ok(calls.includes('desk_press'), calls.join(' '))
  assert.ok(!calls.includes('desk_unanswered'), calls.join(' '))
})
