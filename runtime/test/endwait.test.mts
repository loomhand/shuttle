import assert from 'node:assert/strict'
import { test } from 'node:test'
import { setTimeout as sleep } from 'node:timers/promises'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

type Shown = { title: string; role: string }

function editor(opens: Record<string, Shown> = {}, start: Shown = { title: 'a.txt - Editor', role: 'window' }) {
  const calls: string[] = []
  const wins = [{ id: '1', ...start, pid: process.pid, active: true }]
  const answer = ({ tool, args }: { tool: string; args: Record<string, unknown> }) => {
    const front = wins.at(-1) ?? wins[0]
    calls.push(args.waiting ? 'wait' : tool === 'desk_key' ? `key ${String(args.keys)} on ${front?.title}` : tool)
    if (tool === 'desk_windows') return JSON.stringify(wins.map((w) => ({ ...w, active: w === front })))
    if (tool === 'desk_snapshot') return JSON.stringify({ window: front, truncated: false, nodes: [{ ref: 0, id: `${front?.id}/1`, parent: -1, depth: 0, role: front?.role, name: front?.title, states: ['showing'] }, { ref: 1, id: `${front?.id}/2`, parent: 0, depth: 1, role: 'button', name: 'Cancel', states: ['showing'] }] })
    const opened = tool === 'desk_key' ? opens[String(args.keys)] : undefined
    if (opened) setTimeout(() => { wins.push({ id: String(wins.length + 1), ...opened, pid: process.pid, active: true }) }, 200)
    return '{"ok":true}'
  }
  return { calls, answer }
}

for (const step of [{ key: 'down' }, { key: 'shift+a' }, { type: 'abc' }]) {
  test(`a plan that ends with ${JSON.stringify(step)}, which opens no window, skips the end-of-plan wait`, async (t) => {
    const d = editor()
    await toolhost(t, d.answer)
    const r = await main({ steps: [step] })
    assert.equal(r.ok, true, JSON.stringify(r))
    const act = d.calls.findLastIndex((c) => c.startsWith('key ') || c === 'desk_type')
    assert.equal(d.calls.indexOf('wait', act), -1, d.calls.join(' '))
  })
}

for (const [step, key, title] of [[{ key: 'ctrl+o' }, 'ctrl+o', 'Open'], [{ key: 'alt+f4' }, 'alt+f4', 'Save changes?'], [{ type: 'a\nb' }, 'Return', 'Confirm'], [{ key: 'tab' }, 'tab', 'Invalid value'], [{ key: 'shift+tab' }, 'shift+tab', 'Invalid value']] as const) {
  test(`a plan that ends with ${JSON.stringify(step)} still waits for the dialog it opens late and reports it`, async (t) => {
    const d = editor({ [key]: { title, role: 'dialog' } })
    await toolhost(t, d.answer)
    const r = await main({ steps: [step] })
    assert.equal(r.escalate?.window, title, JSON.stringify(r))
    const act = d.calls.findLastIndex((c) => c.startsWith('key ') || c === 'desk_type')
    assert.notEqual(d.calls.indexOf('wait', act), -1, d.calls.join(' '))
  })
}

for (const [key, start, opened] of [
  ['right', { title: 'File', role: 'menu' }, { title: 'Open Recent', role: 'menu' }],
  ['down', { title: 'Font', role: 'dialog' }, { title: 'Size', role: 'list' }],
] as const) {
  test(`a plan that ends with ${key} in front of a ${start.role} still waits for the ${opened.role} it opens late`, async (t) => {
    const d = editor({ [key]: opened }, start)
    await toolhost(t, d.answer)
    const r = await main({ steps: [{ key }] })
    assert.ok(JSON.stringify(r).includes(opened.title), JSON.stringify(r))
  })
}

test('a popup that opens late after an arrow in a plain window is in front for a plan that starts after it', async (t) => {
  const d = editor({ down: { title: 'Suggestions', role: 'list' } })
  await toolhost(t, d.answer)
  assert.equal((await main({ steps: [{ key: 'down' }] })).ok, true)
  await sleep(300)
  await main({ steps: [{ key: 'escape' }] })
  assert.ok(d.calls.includes('key escape on Suggestions'), d.calls.join(' '))
})
