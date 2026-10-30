import assert from 'node:assert/strict'
import { test } from 'node:test'

import { BUILD, daemon, gray } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'
import { cut, png, unpng } from '../lib/png.mts'

const usage = { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }

function screen(w: number, h: number, at: [number, number]): Buffer {
  const px = Buffer.alloc(w * h * 4, 200)
  let seed = 7
  for (let y = 0; y < 60; y++) for (let x = 0; x < 120; x++) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff
    const i = ((at[1] + y) * w + at[0] + x) * 4
    px[i] = px[i + 1] = px[i + 2] = seed % 256
  }
  return png(w, h, 6, 4, px)
}

type Seen = { clicks: string[]; grounds: number; looks: number }

async function canvas(t: { after(fn: () => void): void }, o: { shown: Buffer; look?: number; title?: (s: Seen) => string; bounds?: [number, number, number, number] }): Promise<Seen> {
  const seen: Seen = { clicks: [], grounds: 0, looks: 0 }
  const win = () => ({ id: '1/1', title: o.title?.(seen) ?? 'Canvas', role: 'window', pid: process.pid, active: true, app: 'Browser', bounds: o.bounds ?? [0, 0, 800, 600] })
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win()])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win(), truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: win().title, states: ['showing'] }, { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'document', name: '', states: ['showing'] }] })
    if (tool === 'desk_screen') return '{"width":800,"height":600}'
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: (args.w === undefined ? o.shown : gray(Number(args.w), Number(args.h))).toString('base64') }
    if (tool === 'desk_at') return JSON.stringify({ found: { ref: 0, id: '1/2', parent: -1, depth: 0, role: 'document', name: '', states: [] }, path: [{ id: '1/1', role: 'window', name: win().title }], pid: process.pid })
    if (tool === 'model_ground') { seen.grounds += 1; return JSON.stringify({ x: 500, y: 300, usage }) }
    if (tool === 'model_decide') {
      const a = args as { questions: Record<string, { type: string }> }
      if (JSON.stringify(args).includes('image_url')) seen.looks += 1
      const answers = Object.fromEntries(Object.keys(a.questions).map((k) => [k, a.questions[k]?.type === 'noul' ? { probability: o.look ?? 0.95 } : { choice: 'none', confidence: 1, probabilities: { none: 1 } }]))
      return JSON.stringify({ answers, served: { model: 'm', build: BUILD, id: null }, usage })
    }
    if (tool === 'desk_click') seen.clicks.push(`${args.x},${args.y}`)
    return '{"ok":true}'
  })
  return seen
}

const crop = (): string => cut(unpng(screen(800, 600, [100, 100])), 60, 80, 200, 100, 1).toString('base64')

test('a recorded click with no element under it finds its recorded crop where the layout moved it, with no model call', async (t) => {
  const seen = await canvas(t, { shown: screen(800, 600, [300, 250]) })
  const r = await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', crop: crop(), at: [160, 130], size: [800, 600], root: true } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.clicks, seen.grounds, r.trace?.[0]?.how], [['360,280'], 0, 'crop'])
})

test('a recorded crop taken at twice the desk scale matches a screen at desk scale', async (t) => {
  const seen = await canvas(t, { shown: screen(800, 600, [300, 250]) })
  const big = cut(unpng(screen(800, 600, [100, 100])), 60, 80, 200, 100, 2).toString('base64')
  const r = await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', crop: big, scale: 2, root: true } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.clicks, r.trace?.[0]?.how], [['360,280'], 'crop'])
})

test('a recorded crop cut at the screen edge clicks its point, not its centre', async (t) => {
  const seen = await canvas(t, { shown: screen(800, 600, [300, 250]) })
  const edge = cut(unpng(screen(800, 600, [100, 100])), 60, 80, 200, 100, 1).toString('base64')
  const r = await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', crop: edge, offset: [-40, 5], root: true } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.clicks, r.trace?.[0]?.how], [['320,285'], 'crop'])
})

test('a recorded click whose target a parameter changed skips the crop and the point, and grounding may land on the content root', async (t) => {
  const seen = await canvas(t, { shown: screen(800, 600, [100, 100]) })
  const r = await main({ steps: [{ click: 'the Blue button', recorded: { text: 'the Green button', crop: crop(), at: [160, 130], size: [800, 600], root: true } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.clicks, seen.grounds, r.trace?.[0]?.how], [['500,300'], 1, 'ground'])
})

test('a plan step that no recording made still refuses a grounded point on the content root', async (t) => {
  const seen = await canvas(t, { shown: screen(800, 600, [100, 100]) })
  const r = await main({ steps: [{ click: 'the Green button' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.deepEqual([seen.clicks, r.trace?.[0]?.ground?.check], [[], 'root-hit'])
})

test('a recorded click takes its recorded point last, and only in a window of the recorded size', async (t) => {
  const seen = await canvas(t, { shown: gray(800, 600), look: 0.1, bounds: [40, 30, 800, 600] })
  const r = await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', crop: crop(), at: [160, 130], size: [800, 600], root: true } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.clicks, r.trace?.[0]?.how], [['200,160'], 'point'])
})

test('a recorded point in a window of another size is no fallback, and the step escalates with what each fallback found', async (t) => {
  const seen = await canvas(t, { shown: gray(800, 600), look: 0.1, bounds: [0, 0, 640, 480] })
  const r = await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', crop: crop(), at: [160, 130], size: [800, 600], root: true } }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.deepEqual(seen.clicks, [])
  assert.match(r.escalate?.reason ?? '', /recorded crop matched nothing.*grounding pointed at/)
})

test('a step whose recorded after-window does not come stops the plan as an error', async (t) => {
  await canvas(t, { shown: screen(800, 600, [300, 250]) })
  const r = await main({ steps: [{ click: 'the Green button', after: 'Green clicked', timeout_ms: 300, recorded: { text: 'the Green button', crop: crop(), root: true } }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(String(r.error), /after this step the recording saw "Green clicked"/)
  assert.equal(r.escalate?.step, 0)
})

test('a step whose recorded after-window comes passes', async (t) => {
  const seen = await canvas(t, { shown: screen(800, 600, [300, 250]), title: (s) => s.clicks.length ? 'Green clicked' : 'Canvas' })
  const r = await main({ steps: [{ click: 'the Green button', after: 'Green clicked', recorded: { text: 'the Green button', crop: crop(), root: true } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(seen.clicks.length, 1)
})
