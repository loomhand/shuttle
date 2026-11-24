import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { BUILD, daemon, gray } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'
import { cut, png, unpng } from '../lib/png.mts'

type Part = string | { image_url?: { url: string } }
type Seen = { shots: Record<string, unknown>[]; asked: string[]; images: number[][]; grounds: unknown[]; prompts: string[]; clicks: string[]; pressed: string[] }

const usage = { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }
const bounds = [-1416, 0, 846, 774]
const win = { id: '1/1', title: 'Portal', role: 'window', pid: process.pid, active: true, bounds }
const root = { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Portal', states: ['showing'] }
const own = { window: 'Portal', pid: process.pid }

function pattern(w: number, h: number, at: [number, number]): Buffer {
  const px = Buffer.alloc(w * h * 4, 200)
  let seed = 7
  for (let y = 0; y < 60; y++) for (let x = 0; x < 120; x++) {
    seed = (seed * 1103515245 + 12345) & 0x7fffffff
    const i = ((at[1] + y) * w + at[0] + x) * 4
    px[i] = px[i + 1] = px[i + 2] = seed % 256
  }
  return png(w, h, 6, 4, px)
}

function size(b64: string): number[] {
  const p = unpng(Buffer.from(b64, 'base64'))
  return [p.w, p.h]
}

async function second(t: TestContext, o: { nodes?: unknown[]; window?: Buffer; read?: (id: string) => string | { text: string; error: boolean }; pick?: Record<string, number>; truth?: number; moves?: number[][] } = {}): Promise<Seen> {
  const seen: Seen = { shots: [], asked: [], images: [], grounds: [], prompts: [], clicks: [], pressed: [] }
  let at = bounds
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...win, bounds: at }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: o.nodes ?? [root] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'desk_read' && o.read) return o.read(String(args.id))
    if (tool === 'desk_shot') {
      seen.shots.push(args)
      if (args.window !== undefined && args.w === undefined) at = o.moves?.shift() ?? at
      const image = args.window === undefined ? gray(Number(args.w ?? 1280), Number(args.h ?? 800)) : args.w === undefined ? o.window ?? gray(846, 774) : gray(Number(args.w), Number(args.h))
      return { text: '{"ok":true}', image: image.toString('base64') }
    }
    if (tool === 'desk_at') return JSON.stringify({ found: { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'image', name: '', states: [] }, path: [{ id: '1/1', role: 'window', name: 'Portal' }], pid: process.pid })
    if (tool === 'model_ground') { seen.grounds.push(args.area); return JSON.stringify({ x: -1000, y: 300, usage }) }
    if (tool === 'model_ask') {
      seen.asked.push(String(args.prompt))
      seen.images.push(...(args.images as string[]).map(size))
      return JSON.stringify({ text: '{"ok": true, "on": true, "why": "it shows"}', usage })
    }
    if (tool === 'model_decide') {
      const a = args as { state: Part[] | unknown; questions: Record<string, { type: string }> }
      if (Array.isArray(a.state)) for (const p of a.state as Part[]) if (typeof p === 'string') seen.prompts.push(p)
      const answers = Object.fromEntries(Object.keys(a.questions).map((k) => [k, a.questions[k]?.type === 'noul' ? { probability: o.truth ?? 0.95 } : { choice: 'r1', confidence: 0.95, probabilities: o.pick ?? { none: 1 } }]))
      return JSON.stringify({ answers, served: { model: 'm', build: BUILD, id: null }, usage })
    }
    if (tool === 'desk_press') { seen.pressed.push(String(args.id)); return '{"ok":true,"how":"click"}' }
    if (tool === 'desk_click') seen.clicks.push(`${args.x},${args.y}`)
    return '{"ok":true}'
  })
  return seen
}

test('an expect on a window of a second display checks the shot of that window alone, and says the image is the window', async (t) => {
  const seen = await second(t, { truth: 0.2 })
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(seen.shots, [own])
  assert.match(seen.asked[0] ?? '', /^You check the state of the app window "Portal", which the image shows alone\.\nCondition: the text is bold\n/)
})

test('a recorded crop on a window of a second display is found in that window\'s shot and clicked in desk coordinates', async (t) => {
  const crop = cut(unpng(pattern(846, 774, [100, 100])), 60, 80, 200, 100, 1).toString('base64')
  const seen = await second(t, { window: pattern(846, 774, [300, 250]) })
  const r = await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', crop, root: true } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.clicks, seen.grounds, r.trace?.[0]?.how], [['-1056,280'], [], 'crop'])
  assert.deepEqual(seen.shots, [own])
})

test('grounding on a window of a second display reads that window\'s shot at its own scale and checks a crop of it in window pixels', async (t) => {
  const seen = await second(t, { window: gray(1692, 1548) })
  const r = await main({ steps: [{ click: 'the yellow star' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(seen.clicks, ['-1000,300'])
  assert.deepEqual(seen.grounds, [{ left: -1416, top: 0, width: 846, height: 774 }])
  assert.deepEqual(seen.shots, [own, { ...own, x: 192, y: 200, w: 1280, h: 800 }])
  assert.match(seen.prompts.find((p) => p.includes('crop')) ?? '', /^A 1280x800 crop of the window\. A click is about to land on its pixel \(640, 400\)\./)
})

test('a grounded check that zooms in on a window of a second display cuts the zoom from that window\'s shot', async (t) => {
  const seen = await second(t, { window: gray(1692, 1548), truth: 0.8 })
  const r = await main({ steps: [{ click: 'the yellow star' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(seen.clicks, ['-1000,300'])
  assert.deepEqual(seen.shots, [own, { ...own, x: 192, y: 200, w: 1280, h: 800 }, own])
  assert.match(seen.prompts.filter((p) => p.includes('crop')).at(-1) ?? '', /^A 384x288 crop of the window\. A click is about to land on its pixel \(192, 144\)\./)
})

test('a state check on a window of a second display crops the control from that window\'s shot', async (t) => {
  const box = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'checkbox', name: 'Bold', states: ['showing'], bounds: [-1000, 100, 20, 20] }
  let reads = 0
  const seen = await second(t, { nodes: [root, box], read: () => reads++ % 2 ? JSON.stringify(box) : { text: 'no state', error: true } })
  const r = await main({ steps: [{ verify: 'Bold', on: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(seen.shots, [own])
  assert.deepEqual(seen.images, [[100, 28]])
})

test('a pick among same-named elements of a window on a second display places them in that window\'s shot', async (t) => {
  const button = (ref: number, x: number) => ({ ref, id: `1/${ref + 1}`, parent: 0, depth: 1, role: 'button', name: 'Send', states: ['showing'], bounds: [x, 100, 100, 40] })
  const seen = await second(t, { nodes: [root, button(1, -1300), button(2, -700)], pick: { r1: 0.95, r2: 0.05, none: 0 } })
  const r = await main({ steps: [{ click: 'Send' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(seen.pressed, ['1/2'])
  assert.deepEqual(seen.shots, [own])
  const list = seen.prompts.find((p) => p.startsWith('Elements')) ?? ''
  assert.match(list, /^Elements of the focused window, with their centres as fractions of the image of the window:\n/)
  assert.match(list, /button "Send" in "Portal" at \(0\.20, 0\.16\)\nbutton "Send" in "Portal" at \(0\.91, 0\.16\)/)
})

test('an expect on an Android device checks the device shot, which takes no window', async (t) => {
  const seen = await second(t, { truth: 0.2 })
  const mode = 'android-expect'
  process.env.SHUTTLE_DESK = 'android'
  const device = ((await import(`../lib/gui_plan.mts?${mode}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_DESK
  const r = await device({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(seen.shots, [{}])
  assert.match(seen.asked[0] ?? '', /^You check the state of a computer screen\./)
})

test('an expect whose window gets a same-titled twin before the shot checks the screen, not a shot by title', async (t) => {
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  let twin = false
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(twin ? [win, { ...win, id: '2/1', active: false }] : [win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'model_decide') { twin = true; return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage }) }
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(1280, 800).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "it shows"}', usage }) }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(shots, [{}])
  assert.match(asked[0] ?? '', /^You check the state of a computer screen\./)
})

test('grounding on a window that moves after its shot grounds again on a fresh shot, and stops when it moves again', async (t) => {
  const moved = [-1400, 20, 846, 774]
  const seen = await second(t, { moves: [moved] })
  const r = await main({ steps: [{ click: 'the yellow star' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(seen.grounds, [{ left: -1416, top: 0, width: 846, height: 774 }, { left: -1400, top: 20, width: 846, height: 774 }])
  assert.deepEqual(seen.clicks, ['-1000,300'])
  const again = await second(t, { moves: [moved, [-1300, 40, 846, 774]] })
  const stopped = await main({ steps: [{ click: 'the yellow star' }] })
  assert.equal(stopped.ok, false, JSON.stringify(stopped))
  assert.match(stopped.escalate?.reason ?? '', /^"Portal" moved, changed size or closed while its target was found in its shot, so nothing was clicked/)
  assert.deepEqual(again.clicks, [])
})

test('an expect while an untitled popup of the plan window is in front checks the screen, which shows the popup', async (t) => {
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  const popup = { id: '9/1', title: '', role: 'window', pid: process.pid, active: true, bounds: [-1300, 120, 300, 200] }
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([popup, { ...win, active: false }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage })
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(1280, 800).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "the suggestions show apple"}', usage }) }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: "the suggestions show 'apple'" }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(shots, [{}])
  assert.match(asked[0] ?? '', /^You check the state of a computer screen\./)
})

test('a recorded point fallback places its point by where the window is now, not where it was at the step start', async (t) => {
  const clicks: string[] = []
  let at = [0, 0, 800, 600]
  const pad = { id: '3/1', title: 'Canvas', role: 'window', pid: process.pid, active: true }
  const hit = { ref: 1, id: '3/2', parent: 0, depth: 1, role: 'button', name: 'Green', states: [] }
  const page = { ref: 1, id: '3/3', parent: 0, depth: 1, role: 'document', name: '', states: [] }
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...pad, bounds: at }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: pad, truncated: false, nodes: [{ ref: 0, id: '3/1', parent: -1, depth: 0, role: 'window', name: 'Canvas', states: ['showing'] }] })
    if (tool === 'desk_screen') return '{"width":800,"height":600}'
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(Number(args.w ?? 800), Number(args.h ?? 600)).toString('base64') }
    if (tool === 'model_ground') { at = [100, 50, 800, 600]; return JSON.stringify({ x: 500, y: 300, usage }) }
    if (tool === 'desk_at') return JSON.stringify({ found: args.x === 260 && args.y === 180 ? hit : page, path: [{ id: '3/1', role: 'window', name: 'Canvas' }], pid: process.pid })
    if (tool === 'model_decide') return JSON.stringify({ answers: { on: { probability: 0.95 } }, served: { model: 'm', build: BUILD, id: null }, usage })
    if (tool === 'desk_click') clicks.push(`${args.x},${args.y}`)
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', at: [160, 130], size: [800, 600] } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([clicks, r.trace?.[0]?.how], [['260,180'], 'point'])
})
