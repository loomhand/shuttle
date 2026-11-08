import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { daemon } from './daemon.mts'
import { main as plan } from '../lib/gui_plan.mts'

test('a Linux step that only ran an accessibility action carries a caveat that no later step read its effect, and does not escalate', { skip: process.platform !== 'linux' }, async (t) => {
  const win = { id: '1.14/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 889, active: true }
  const nodes = [
    { ref: 0, id: '1.14/117', parent: -1, depth: 0, role: 'window', name: 'Untitled 1 - Mousepad', states: ['showing'] },
    { ref: 1, id: '1.14/400', parent: 0, depth: 1, role: 'button', name: 'Save', states: ['showing'], bounds: [420, 300, 80, 30] },
  ]
  let how = 'action'
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes, truncated: false })
    if (tool === 'desk_press') return JSON.stringify({ ok: true, how })
    return '{"ok":true}'
  })
  const blind = await plan({ steps: [{ click: 'Save' }] })
  assert.equal(blind.ok, true, JSON.stringify(blind))
  assert.equal(blind.escalate, undefined)
  assert.deepEqual(blind.caveats?.map((c) => c.step), [0], JSON.stringify(blind))
  const read = await plan({ steps: [{ click: 'Save' }, { verify: 'Save', enabled: true }] })
  assert.equal(read.ok, true, JSON.stringify(read))
  assert.equal(read.caveats, undefined, JSON.stringify(read))
  how = 'select'
  const selected = await plan({ steps: [{ click: 'Save' }] })
  assert.deepEqual(selected.caveats?.map((c) => c.step), [0], JSON.stringify(selected))
  how = 'click'
  const clicked = await plan({ steps: [{ click: 'Save' }] })
  assert.equal(clicked.ok, true, JSON.stringify(clicked))
  assert.equal(clicked.caveats, undefined, JSON.stringify(clicked))
})

test('a Linux accessibility action that moves the focus late, as a GTK 4 button does, is waited out before the next step types or presses a key', { skip: process.platform !== 'linux' }, async (t) => {
  const got = await newTab(t, 300)
  assert.deepEqual(got.map((g) => g.into), ['some text -> new tab', 'ctrl+v -> new tab'])
})

test('a Linux accessibility action that leaves the focus where it was delays the next type or key step by half a second at most', { skip: process.platform !== 'linux' }, async (t) => {
  const got = await newTab(t, Infinity)
  assert.deepEqual(got.map((g) => g.into), ['some text -> old tab', 'ctrl+v -> old tab'])
  assert.ok(got.every((g) => g.ms < 700), JSON.stringify(got))
})

test('a shown match in a tree without bounds, as GTK 4 gives, is clicked without a model pick', async (t) => {
  const win = { id: '1.20/5', title: 'Untitled - Text Editor', role: 'window', pid: 901, active: true }
  const nodes = [
    { ref: 0, id: '1.20/5', parent: -1, depth: 0, role: 'window', name: 'Untitled - Text Editor', states: ['showing'] },
    { ref: 1, id: '1.20/6', parent: 0, depth: 1, role: 'button', name: 'Save', states: ['showing'] },
    { ref: 2, id: '1.20/7', parent: 0, depth: 1, role: 'button', name: 'Open', states: ['showing'] },
  ]
  const tools: string[] = []
  await daemon(t, ({ tool }) => {
    tools.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes, truncated: false })
    if (tool === 'desk_press') return JSON.stringify({ ok: true, how: 'click' })
    return '{"ok":true}'
  })
  const r = await plan({ steps: [{ click: 'Save' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.trace?.[0]?.how, 'ax', JSON.stringify(r.trace))
  assert.ok(!tools.some((x) => x.startsWith('model_')), tools.join(' '))
})

async function newTab(t: TestContext, after: number) {
  const win = { id: '1.20/5', title: 'New Document (Draft) - Text Editor', role: 'window', pid: 901, active: true }
  let pressed = 0
  const moved = () => pressed > 0 && Date.now() - pressed > after
  const view = (id: string, on: boolean) => ({ ref: 0, id, parent: 0, depth: 1, role: 'textbox', name: '', states: on ? ['showing', 'focused'] : ['showing'] })
  const nodes = () => [
    { ref: 0, id: '1.20/5', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] },
    { ref: 1, id: '1.20/6', parent: 0, depth: 1, role: 'button', name: 'New tab', states: ['showing'] },
    { ...view('1.20/7', !moved()), ref: 2 },
    ...(moved() ? [{ ...view('1.20/8', true), ref: 3 }] : []),
  ]
  const got: { into: string; ms: number }[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes: nodes(), truncated: false })
    if (tool === 'desk_read') return JSON.stringify(nodes().find((n) => n.id === args.id))
    if (tool === 'desk_press') { pressed = Date.now(); return '{"ok":true,"how":"action"}' }
    if (tool === 'desk_type' || tool === 'desk_key') got.push({ into: `${String(args.text ?? args.keys)} -> ${moved() ? 'new tab' : 'old tab'}`, ms: Date.now() - pressed })
    return '{"ok":true}'
  })
  const typed = await plan({ steps: [{ click: 'New tab' }, { type: 'some text' }] })
  assert.equal(typed.ok, true, JSON.stringify(typed))
  pressed = 0
  const keyed = await plan({ steps: [{ click: 'New tab' }, { key: 'ctrl+v' }] })
  assert.equal(keyed.ok, true, JSON.stringify(keyed))
  return got
}
