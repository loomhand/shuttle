import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

async function unfocused(t: TestContext, titles: string[], role = 'dialog', busy: object | null = null) {
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
    if (tool === 'desk_unanswered') return JSON.stringify(busy)
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

test('with no window in front, a plan names the front app that is not answering accessibility', async (t) => {
  await unfocused(t, [], 'dialog', { id: '', title: 'answers.xlsx - LibreOffice Calc', role: 'window', pid: 5173, active: false, app: 'soffice.bin' })
  const busy = '(pid 5173) is not answering accessibility'
  const clicked = await main({ steps: [{ click: 'Save' }] })
  assert.equal(clicked.escalate?.reason, `front app "soffice.bin" ${busy}`, JSON.stringify(clicked))
  const waited = await main({ steps: [{ wait: 'Keep current format', timeout_ms: 400 }] })
  assert.equal(waited.escalate?.reason, 'expected window "Keep current format" did not appear', JSON.stringify(waited))
  assert.equal(waited.escalate?.outline, `(front app "soffice.bin" ${busy})`, JSON.stringify(waited))
})
