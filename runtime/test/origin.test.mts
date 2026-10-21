import assert from 'node:assert/strict'
import { test } from 'node:test'

import { BUILD, gray, daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

type Part = string | { image_url?: { url: string } }

test('a grounded check on a screen whose origin is left of zero crops around the point instead of clamping it to zero', async (t) => {
  const win = { id: '1.1/1', title: 'Portal', role: 'frame', pid: process.pid, active: true }
  const usage = { model: 'decider', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }
  const prompts: string[] = []
  const clicked: string[] = []
  const shots: Record<string, unknown>[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes: [{ ref: 0, id: '1.1/1', parent: -1, depth: 0, role: 'frame', name: 'Portal', states: ['showing'], bounds: [0, 0, 10, 10] }], truncated: false })
    if (tool === 'desk_screen') return '{"left":-1280,"top":0,"width":2560,"height":800}'
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(Number(args.w ?? 2560), Number(args.h ?? 800)).toString('base64') } }
    if (tool === 'desk_at') return JSON.stringify({ found: { role: 'image', name: '' }, path: [], pid: process.pid })
    if (tool === 'model_ground') return JSON.stringify({ x: 0, y: 400, usage })
    if (tool === 'model_decide') {
      const a = args as { state: Part[]; questions: Record<string, { type: string }> }
      for (const p of a.state ?? []) if (typeof p === 'string') prompts.push(p)
      const answers = Object.fromEntries(Object.keys(a.questions).map((k) => [k, a.questions[k]?.type === 'noul' ? { probability: 0.92 } : { choice: 'none', confidence: 1, probabilities: { none: 1 } }]))
      return JSON.stringify({ answers, served: { model: 'decider', build: BUILD, id: null }, usage })
    }
    if (tool === 'desk_click') clicked.push(`${args.x},${args.y}`)
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'the yellow star' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(clicked, ['0,400'])
  assert.match(prompts.find((p) => p.includes('crop')) ?? '', /640x400 crop.*pixel \(320, 200\)/)
  assert.deepEqual(shots.at(-1), { x: 960, y: 200, w: 640, h: 400 })
})
