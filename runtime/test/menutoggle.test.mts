import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { daemon, gray } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Untitled - Editor', role: 'window', pid: process.pid, active: true }
const usage = { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }

async function editor(t: TestContext, role: string, flips: boolean) {
  const state = { on: false }
  const item = () => ({ ref: 3, id: '1/4', parent: 2, depth: 3, role, name: 'Status Bar', states: state.on ? ['checked'] : [], bounds: [100, 20, 80, 20] })
  const nodes = () => [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'View', states: ['showing'], bounds: [100, 0, 40, 20] },
    item(),
  ]
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: nodes() })
    if (tool === 'desk_read') return JSON.stringify(item())
    if (tool === 'desk_press') { if (args.id === '1/4' && flips) state.on = !state.on; return '{"ok":true,"how":"click"}' }
    if (tool === 'desk_shot') return { text: '', image: gray(120, 28).toString('base64') }
    if (tool === 'model_ask') return JSON.stringify({ text: JSON.stringify({ on: state.on }), usage })
    return '{"ok":true}'
  })
  return state
}

const step = { menu: 'View > Status Bar', on: true }

for (const flips of [true, false]) {
  test(`a menu toggle reads its state back after the press, which ${flips ? 'took' : 'did not take'}`, async (t) => {
    const state = await editor(t, process.platform === 'darwin' ? 'menuitem' : 'menuitemcheckbox', flips)
    const r = await main({ steps: [step] })
    assert.equal(r.ok, flips, JSON.stringify(r))
    if (flips) assert.equal(r.trace?.[0]?.now, 'on', JSON.stringify(r))
    else assert.match(r.escalate?.reason ?? '', /"Status Bar" is still off/)
    assert.equal(state.on, flips)
  })
}

test('a menu toggle whose state only pixels show reports no state after its press and carries a caveat', { skip: process.platform === 'darwin' }, async (t) => {
  await editor(t, 'menuitem', true)
  const r = await main({ steps: [step] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.trace?.[0]?.now, undefined, JSON.stringify(r))
  assert.deepEqual(r.caveats?.map((c) => c.step), [0], JSON.stringify(r))
})

test('a Windows menu toggle read by pixels keeps the menu the plan opened and never presses its popup', { skip: process.platform !== 'win32' }, async (t) => {
  const state = { open: false, on: true }
  const presses: string[] = []
  const nodes = () => [
    { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] },
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menubar', name: 'Application', states: ['showing'] },
    { ref: 2, id: '1/3', parent: 1, depth: 2, role: 'menu', name: 'View', states: ['showing'], bounds: [100, 0, 40, 20] },
    ...(state.open ? [
      { ref: 3, id: 'p/1', parent: 0, depth: 1, role: 'menu', name: 'View', states: ['showing'], bounds: [100, 20, 140, 44] },
      { ref: 4, id: 'p/2', parent: 3, depth: 2, role: 'menuitem', name: 'Status Bar', states: ['showing'], bounds: [100, 20, 140, 22] },
    ] : []),
  ]
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: nodes() })
    if (tool === 'desk_read') { const n = nodes().find((x) => x.id === args.id); return n ? JSON.stringify(n) : { text: `no element ${String(args.id)}`, error: true } }
    if (tool === 'desk_press') {
      presses.push(String(args.id))
      if (args.id === '1/3') state.open = !state.open
      if (args.id === 'p/1' || args.id === 'p/2') { state.on = !state.on; state.open = false }
      return '{"ok":true,"how":"click"}'
    }
    if (tool === 'desk_shot') return { text: '', image: gray(120, 28).toString('base64') }
    if (tool === 'model_ask') return JSON.stringify({ text: JSON.stringify({ on: state.on }), usage })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ menu: 'View > Status Bar', on: false }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(presses, ['1/3', 'p/2'])
  assert.equal(state.on, false)
})
