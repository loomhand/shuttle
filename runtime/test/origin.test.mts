import assert from 'node:assert/strict'
import { test } from 'node:test'
import { crc32, deflateSync } from 'node:zlib'

import { toolhost } from './toolhost.mts'

type Part = string | { image_url?: { url: string } }

test('a grounded check on a screen whose origin is left of zero crops around the point instead of clamping it to zero', async () => {
  const win = { id: '1.1/1', title: 'Portal', role: 'frame', pid: process.pid, active: true }
  const usage = { model: 'decider', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }
  const prompts: string[] = []
  const clicked: string[] = []
  const shots: Record<string, unknown>[] = []
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes: [{ ref: 0, id: '1.1/1', parent: -1, depth: 0, role: 'frame', name: 'Portal', states: ['showing'], bounds: [0, 0, 10, 10] }], truncated: false })
    if (tool === 'desk_screen') return '{"left":-1280,"top":0,"width":2560,"height":800}'
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(Number(args.w ?? 2560), Number(args.h ?? 800)).toString('base64') } }
    if (tool === 'desk_at') return '{"found":null,"path":[]}'
    if (tool === 'model_ground') return JSON.stringify({ x: 0, y: 400, usage })
    if (tool === 'model_decide') {
      const a = args as { state: Part[]; questions: Record<string, { type: string }> }
      for (const p of a.state ?? []) if (typeof p === 'string') prompts.push(p)
      const answers = Object.fromEntries(Object.keys(a.questions).map((k) => [k, a.questions[k]?.type === 'noul' ? { probability: 0.8 } : { choice: 'none', confidence: 1, probabilities: { none: 1 } }]))
      return JSON.stringify({ answers, served: { model: 'decider', build: 'typesafe/jev-1.14-20261014', id: null }, usage })
    }
    if (tool === 'desk_click') clicked.push(`${args.x},${args.y}`)
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ click: 'the yellow star' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.deepEqual(clicked, ['0,400'])
    assert.match(prompts.find((p) => p.includes('crop')) ?? '', /640x400 crop.*pixel \(320, 200\)/)
    assert.deepEqual(shots.at(-1), { x: 960, y: 200, w: 640, h: 400 })
  } finally {
    server.close()
  }
})

function gray(w: number, h: number) {
  const chunk = (kind: string, data: Buffer) => {
    const head = Buffer.alloc(8), crc = Buffer.alloc(4); head.writeUInt32BE(data.length); head.write(kind, 4, 'ascii')
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data]))); return Buffer.concat([head, data, crc])
  }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 6
  const raw = Buffer.alloc(h * (w * 4 + 1), 128); for (let y = 0; y < h; y++) raw[y * (w * 4 + 1)] = 0
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))])
}
