import assert from 'node:assert/strict'
import { test } from 'node:test'

import { BUILD, gray, daemon, type Call } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const SPENT = 'model credits are used up: typesafe/jev-1.14 at https://gw.example/v1/decisions returned 402 Payment Required; add credit or upgrade the plan, then try again'
const win = { id: '1.1/1', title: 'Portal', role: 'frame', pid: process.pid, active: true }
const frame = { ref: 0, id: '1.1/1', parent: -1, depth: 0, role: 'frame', name: 'Portal', states: ['showing'], bounds: [0, 0, 1280, 800] }
const button = { ref: 1, id: '1.1/2', parent: 0, depth: 1, role: 'button', name: 'Starry thing', states: ['showing'], bounds: [100, 100, 40, 20] }
const usage = { model: 'decider', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }

async function plan(t: { after(fn: () => void): void }, nodes: unknown[], model: (c: Call) => string | null) {
  const calls: string[] = []
  const clicked: unknown[] = []
  await daemon(t, (c) => {
    const { tool, args } = c
    if (tool.startsWith('model_')) {
      calls.push(tool)
      const r = model(c)
      return r ?? { text: SPENT, error: true }
    }
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes, truncated: false })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(Number(args.w ?? 1280), Number(args.h ?? 800)).toString('base64') }
    if (tool === 'desk_at') return JSON.stringify({ found: { id: '1.1/3', role: 'image', name: 'star' }, path: [{ id: win.id, role: 'frame', name: 'Portal' }], pid: process.pid })
    if (tool === 'desk_click' || tool === 'desk_press' || tool === 'desk_act') clicked.push(args)
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'the yellow star' }] })
  return { r, calls, clicked }
}

function stopped(r: Awaited<ReturnType<typeof main>>) {
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.error, SPENT, JSON.stringify(r))
  assert.equal(r.escalate?.reason, SPENT)
  assert.equal(r.done, 0)
}

test('a 402 on the grounding of a click ends the plan with the credits message and clicks nothing', async (t) => {
  const { r, calls, clicked } = await plan(t, [frame], () => null)
  stopped(r)
  assert.deepEqual(calls, ['model_ground'])
  assert.deepEqual(clicked, [])
})

test('a 402 on the check of a grounded point ends the plan without a fallback model call or a click', async (t) => {
  const { r, calls, clicked } = await plan(t, [frame], ({ tool }) => tool === 'model_ground' ? JSON.stringify({ x: 640, y: 400, usage }) : null)
  stopped(r)
  assert.deepEqual(calls, ['model_ground', 'model_decide'])
  assert.deepEqual(clicked, [])
})

test('a 402 on the screenshot pick of an element ends the plan without falling back to the text pick', async (t) => {
  const { r, calls, clicked } = await plan(t, [frame, button], ({ args }) => (args as { image?: boolean }).image ? null : JSON.stringify({ answers: { pick: { choice: 'r1', confidence: 1, probabilities: { r1: 1 } } }, served: { model: 'decider', build: BUILD, id: null }, usage }))
  stopped(r)
  assert.deepEqual(calls, ['model_decide'])
  assert.deepEqual(clicked, [])
})
