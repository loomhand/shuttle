import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { BUILD, daemon, gray } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'
import { cut, png, unpng } from '../lib/png.mts'

type Part = string | { image_url?: { url: string } }
type Seen = { shots: Record<string, unknown>[]; asked: string[]; images: number[][]; tones: number[]; grounds: unknown[]; prompts: string[]; clicks: string[]; pressed: string[] }

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

async function second(t: TestContext, o: { nodes?: unknown[]; window?: Buffer; read?: (id: string) => string | { text: string; error: boolean }; pick?: Record<string, number>; truth?: number; moves?: number[][]; readMoves?: number[] } = {}): Promise<Seen> {
  const seen: Seen = { shots: [], asked: [], images: [], tones: [], grounds: [], prompts: [], clicks: [], pressed: [] }
  let at = bounds
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...win, bounds: at }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: o.nodes ?? [root] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'desk_read' && o.readMoves) at = o.readMoves
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
      seen.tones.push(...(args.images as string[]).map((b64) => unpng(Buffer.from(b64, 'base64')).px[0] ?? -1))
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
  const here = { ...win, bounds: [0, 0, 846, 774] }
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(twin ? [here, { ...here, id: '2/1', active: false }] : [here])
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'desk_snapshot') return JSON.stringify({ window: here, truncated: false, nodes: [root] })
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
  const popup = { id: '9/1', title: '', role: 'window', pid: process.pid, active: true, bounds: [100, 120, 300, 200] }
  const here = { ...win, bounds: [0, 0, 846, 774] }
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([popup, { ...here, active: false }])
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'desk_snapshot') return JSON.stringify({ window: here, truncated: false, nodes: [root] })
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

test('a state check crops the control from the window where the window list places it now', async (t) => {
  const box = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'checkbox', name: 'Bold', states: ['showing'], bounds: [-1000, 100, 20, 20] }
  const px = Buffer.alloc(846 * 774 * 4, 200)
  for (let y = 0; y < 774; y++) for (let x = 360; x < 376; x++) px.fill(0, (y * 846 + x) * 4, (y * 846 + x) * 4 + 4)
  let reads = 0
  const seen = await second(t, { nodes: [root, box], window: png(846, 774, 6, 4, px), readMoves: [-1400, 20, 846, 774], read: () => reads++ % 2 ? JSON.stringify(box) : { text: 'no state', error: true } })
  const r = await main({ steps: [{ verify: 'Bold', on: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.images, seen.tones], [[[100, 28]], [0]])
})

test('an expect whose window retitles itself before the shot shoots it by its current title', async (t) => {
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  let title = 'Portal'
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...win, title }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'model_decide') { title = 'Portal - edited'; return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage }) }
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(846, 774).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "it shows"}', usage }) }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(shots, [{ window: 'Portal - edited', pid: process.pid }])
  assert.match(asked[0] ?? '', /^You check the state of the app window "Portal - edited", which the image shows alone\./)
})

test('an expect while an untitled window of the plan window\'s app covers it checks the screen, and a titled one beside it keeps the window shot', async (t) => {
  const shots: Record<string, unknown>[] = []
  let other = { id: '9/1', title: '', role: 'window', pid: process.pid, active: false, bounds: [100, 120, 300, 200] }
  const here = { ...win, bounds: [0, 0, 846, 774] }
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([other, here])
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'desk_snapshot') return JSON.stringify({ window: here, truncated: false, nodes: [root] })
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage })
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(846, 774).toString('base64') } }
    if (tool === 'model_ask') return JSON.stringify({ text: '{"ok": true, "why": "it shows"}', usage })
    return '{"ok":true}'
  })
  const list = await main({ steps: [{ expect: "the suggestions show 'apple'" }] })
  assert.equal(list.ok, true, JSON.stringify(list))
  other = { ...other, title: 'Untitled 2', bounds: [26, 20, 846, 774] }
  const cascade = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(cascade.ok, true, JSON.stringify(cascade))
  assert.deepEqual(shots, [{}, own])
})

test('a window that moves after its shot and whose next shot fails never takes a click at its old place', async (t) => {
  const clicks: string[] = []
  let at = [0, 0, 800, 600]; let shots = 0
  const pad = { id: '4/1', title: 'Sketch', role: 'window', pid: process.pid, active: true }
  const crop = cut(unpng(pattern(800, 600, [100, 100])), 60, 80, 200, 100, 1).toString('base64')
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...pad, bounds: at }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: pad, truncated: false, nodes: [{ ref: 0, id: '4/1', parent: -1, depth: 0, role: 'window', name: 'Sketch', states: ['showing'] }] })
    if (tool === 'desk_screen') return '{"width":800,"height":600}'
    if (tool === 'desk_shot' && args.window !== undefined && args.w === undefined) {
      shots += 1
      if (shots > 1) return { text: 'the window shot failed', error: true }
      at = [100, 50, 800, 600]
    }
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(Number(args.w ?? 800), Number(args.h ?? 600)).toString('base64') }
    if (tool === 'model_ground') return JSON.stringify({ x: 500, y: 300, usage })
    if (tool === 'desk_at') return JSON.stringify({ found: { ref: 1, id: '4/2', parent: 0, depth: 1, role: 'image', name: '', states: [] }, path: [{ id: '4/1', role: 'window', name: 'Sketch' }], pid: process.pid })
    if (tool === 'model_decide') return JSON.stringify({ answers: { on: { probability: 0.95 } }, served: { model: 'm', build: BUILD, id: null }, usage })
    if (tool === 'desk_click') clicks.push(`${args.x},${args.y}`)
    return '{"ok":true}'
  })
  await main({ steps: [{ click: 'the Green button', recorded: { text: 'the Green button', crop, at: [160, 130], size: [800, 600] } }] })
  assert.ok(!clicks.includes('160,130'), JSON.stringify(clicks))
  assert.deepEqual(clicks, ['260,180'])
})

async function toggled(t: TestContext, after: (w: typeof win) => object[]) {
  const main1 = { ...win, bounds: [0, 0, 846, 774] }
  const box = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'checkbox', name: 'Bold', states: ['showing'], bounds: [300, 100, 20, 20] }
  const shots: Record<string, unknown>[] = []
  let reads = 0; let read = false
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(read ? after(main1) : [main1])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: main1, truncated: false, nodes: [root, box] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'desk_read') { read = true; return reads++ % 2 ? JSON.stringify(box) : { text: 'no state', error: true } }
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(Number(args.w ?? 846), Number(args.h ?? 774)).toString('base64') } }
    if (tool === 'model_ask') return JSON.stringify({ text: '{"on": true}', usage })
    return '{"ok":true}'
  })
  return shots
}

test('a state check whose window got a same-titled twin since the step began does not shoot by title', async (t) => {
  const shots = await toggled(t, (w) => [w, { ...w, id: '2/1', active: false }])
  const r = await main({ steps: [{ verify: 'Bold', on: true }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.ok(shots.every((a) => a.window === undefined), JSON.stringify(shots))
})

test('a state check whose window closed before the read stops and crops no screen', async (t) => {
  const shots = await toggled(t, () => [])
  const r = await main({ steps: [{ verify: 'Bold', on: true }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /^"Portal" closed before the state of "Bold" was read$/)
  assert.deepEqual(shots, [])
})

test('an expect that would fall back to the main display while its window is on another one, wholly or in part, stops as unverifiable', async (t) => {
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  let at = bounds
  await daemon(t, ({ tool, args }) => {
    const popup = { id: '9/1', title: '', role: 'window', pid: process.pid, active: false, bounds: [(at[0] ?? 0) + 116, 120, 300, 200] }
    if (tool === 'desk_windows') return JSON.stringify([popup, { ...win, bounds: at }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage })
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(1280, 800).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "it is gone"}', usage }) }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: 'the completion list is gone' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /^the condition cannot be checked: the screen shot of the main display cannot show every window the check needs: the completion list is gone$/)
  assert.deepEqual([shots, asked], [[], []])
  at = [-400, 0, 846, 774]
  const partly = await main({ steps: [{ expect: 'the completion list is gone' }] })
  assert.equal(partly.ok, false, JSON.stringify(partly))
  assert.equal(partly.escalate?.reason, r.escalate?.reason)
  assert.deepEqual([shots, asked], [[], []])
})

test('an expect on a window that straddles the main display with a focused untitled popup over it stops as unverifiable', async (t) => {
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  const here = { ...win, active: false, bounds: [-300, 100, 846, 774] }
  const popup = { id: '9/1', title: '', role: 'window', pid: process.pid, active: true, bounds: [-100, 300, 300, 200] }
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([popup, here])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: here, truncated: false, nodes: [root] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage })
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(1280, 800).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "the list shows apple"}', usage }) }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: "the suggestions show 'apple'" }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, "the condition cannot be checked: the screen shot of the main display cannot show every window the check needs: the suggestions show 'apple'")
  assert.deepEqual([shots, asked], [[], []])
})

async function offMain(t: TestContext, before: number[], after: number[], popup: number[], focused = false) {
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  let moved = false
  await daemon(t, ({ tool, args }) => {
    const pop = { id: '9/1', title: '', role: 'window', pid: process.pid, active: moved && focused, bounds: popup }
    if (tool === 'desk_windows') return JSON.stringify(moved ? [pop, { ...win, active: !focused, bounds: after }] : [{ ...win, bounds: before }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'model_decide') { moved = true; return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage }) }
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(1280, 800).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "it is gone"}', usage }) }
    return '{"ok":true}'
  })
  return { shots, asked }
}

test('an expect whose window moved off the main display during the tree check stops as unverifiable', async (t) => {
  const seen = await offMain(t, [100, 0, 846, 774], [-1416, 0, 846, 774], [-1300, 120, 300, 200])
  const r = await main({ steps: [{ expect: 'the completion list is gone' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /^the condition cannot be checked: /)
  assert.deepEqual([seen.shots, seen.asked], [[], []])
})

test('an expect on a straddling window whose popup lies wholly on another display stops as unverifiable', async (t) => {
  for (const focused of [false, true]) {
    const seen = await offMain(t, [-300, 100, 846, 774], [-300, 100, 846, 774], [-280, 300, 200, 150], focused)
    const r = await main({ steps: [{ expect: 'the completion list is gone' }] })
    assert.equal(r.ok, false, `focused ${focused}: ${JSON.stringify(r)}`)
    assert.match(r.escalate?.reason ?? '', /^the condition cannot be checked: /)
    assert.deepEqual([seen.shots, seen.asked], [[], []])
  }
})

test('an expect on a window off the main display that has a same-titled twin stops with the neutral reason', async (t) => {
  const shots: Record<string, unknown>[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win, { ...win, id: '2/1', active: false }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'model_decide') return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage })
    if (tool === 'desk_shot') shots.push(args)
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, 'the condition cannot be checked: the screen shot of the main display cannot show every window the check needs: the text is bold')
  assert.deepEqual(shots, [])
})

async function fresh(t: TestContext, after: () => object[]) {
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  let read = false
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(read ? after() : [win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root] })
    if (tool === 'desk_screen') return '{"width":1280,"height":800}'
    if (tool === 'model_decide') { read = true; return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage }) }
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(1280, 800).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "it shows"}', usage }) }
    return '{"ok":true}'
  })
  return { shots, asked }
}

test('an expect on a plan window on the main display while another app holds the focus on a second display checks the screen', async (t) => {
  const here = { ...win, active: false, bounds: [0, 0, 846, 774] }
  const elsewhere = { id: '7/1', title: 'Notes', role: 'window', pid: 4242, active: true, bounds: [-1416, 0, 800, 600] }
  const seen = await fresh(t, () => [elsewhere, here])
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.shots, seen.asked.length], [[{}], 1])
})

test('an expect whose window closed during the tree check stops before any shot or vision call', async (t) => {
  const other = { id: '8/1', title: 'Other document', role: 'window', pid: process.pid, active: true, bounds: [0, 0, 846, 774] }
  const seen = await fresh(t, () => [other])
  const r = await main({ steps: [{ expect: 'the status indicator is green' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, '"Portal" closed before the condition was checked: the status indicator is green')
  assert.deepEqual([seen.shots, seen.asked], [[], []])
})

test('an expect on a plan window on the main display while a sheet of another window of the app holds the focus on a second display checks the screen', async (t) => {
  const here = { ...win, active: false, bounds: [0, 0, 846, 774] }
  const doc = { id: '4/1', title: 'Doc 2', role: 'window', pid: process.pid, active: false, bounds: [-1416, 0, 800, 600] }
  const sheet = { id: '5/1', title: '', role: 'dialog', pid: process.pid, active: true, owner: '4/1', bounds: [-1316, 22, 600, 300] }
  const seen = await fresh(t, () => [sheet, doc, here])
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([seen.shots, seen.asked.length], [[{}], 1])
})

test('an expect whose window without bounds is listed again under another id keeps the screen shot', async (t) => {
  const { bounds: _, ...bare } = win
  const again = { ...bare, id: '1/2' }
  const shots: Record<string, unknown>[] = []; const asked: string[] = []
  let read = false
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([read ? again : bare])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: bare, truncated: false, nodes: [root] })
    if (tool === 'model_decide') { read = true; return JSON.stringify({ answers: { ok: { probability: 0.2 } }, served: { model: 'm', build: BUILD, id: null }, usage }) }
    if (tool === 'desk_shot') { shots.push(args); return { text: '{"ok":true}', image: gray(1280, 800).toString('base64') } }
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: '{"ok": true, "why": "it shows"}', usage }) }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ expect: 'the text is bold' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual([shots, asked.length], [[{}], 1])
})
