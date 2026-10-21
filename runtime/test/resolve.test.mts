import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { after, beforeEach, test } from 'node:test'

import { BUILD, gray, daemon, type Call, type Reply } from './daemon.mts'
import type { Size } from '../lib/desk.mts'
import { main, type Result } from '../lib/gui_plan.mts'
import { unpng } from '../lib/png.mts'

type El = { ref: number; id: string; parent: number; depth: number; role: string; name: string; states: string[]; bounds?: number[] }
type Spec = Omit<El, 'ref' | 'depth' | 'states'> & { states?: string[] }
type Part = string | { image_url?: { url: string } }
type Decide = { state: unknown; questions: Record<string, { type: string }>; image: boolean }
type RunStep = { checked?: string; p?: number; p_zoom?: number | null; p_full?: number | null; served?: unknown[] }

const win = { id: '1.1/1', title: 'Portal', role: 'frame', pid: process.pid, active: true }
const nodes: El[] = []
const shell: { win: { id: string; title: string; role: string; pid: number; active: boolean }; nodes: El[] }[] = []
const pressed: string[] = []
const clicked: string[] = []
const atPids: unknown[] = []
const asked: { image: boolean }[] = []
const looks: string[] = []
const shades: number[] = []
const prompts: string[] = []
const usage = { model: 'decider', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }
let shots = 0
let tones: number[] = []
let scale = 1
let pick: Record<string, number> = {}
let looked: Record<string, number> = {}
let build: string | null = null
let on: number | number[] = 0
let under: { role: string; name: string; bounds?: number[] } | null = null
let hitPid: number | null = process.pid
let yes = 0

function answer({ tool, args }: Call): Reply {
  if (tool === 'model_decide') return decide(args as Decide)
  if (tool === 'model_ground') {
    const a = args.area as Size
    return JSON.stringify({ x: (a.left ?? 0) + a.width / 2, y: (a.top ?? 0) + a.height / 2, usage })
  }
  if (tool === 'model_ask') return JSON.stringify({ text: '{"on": false}', usage })
  if (tool === 'desk_windows') return JSON.stringify([win, ...shell.map((x) => x.win)])
  if (tool === 'desk_snapshot') {
    const x = shell.find((y) => y.win.id === args.id)
    return JSON.stringify(x ? { window: x.win, nodes: x.nodes, truncated: false } : { window: win, nodes, truncated: false })
  }
  if (tool === 'desk_press') { pressed.push(String(args.id)); return '{"ok":true,"how":"click"}' }
  if (tool === 'desk_shot') {
    shots += 1
    const image = gray(Number(args.w ?? 1280) * scale, Number(args.h ?? 800) * scale, tones.shift() ?? 128)
    return { text: '{"ok":true}', image: image.toString('base64') }
  }
  if (tool === 'desk_screen') return '{"width":1280,"height":800}'
  if (tool === 'desk_at') { atPids.push(args.pid); return JSON.stringify({ found: under, path: [], pid: hitPid }) }
  if (tool === 'desk_click') clicked.push(`${args.x},${args.y}`)
  return '{"ok":true}'
}

function decide({ state, questions, image }: Decide): Reply {
  asked.push({ image })
  if (image && !build) return { text: 'the verify model failed', error: true }
  if (image) looks.push((state as Part[]).flatMap((p) => typeof p === 'object' && p.image_url ? [size(Buffer.from(p.image_url.url.split(',')[1] ?? '', 'base64'))] : []).join(' '))
  if (image) for (const p of state as Part[]) {
    if (typeof p === 'string') prompts.push(p)
    else if (p.image_url) shades.push(unpng(Buffer.from(p.image_url.url.split(',')[1] ?? '', 'base64')).px.readUInt8(0))
  }
  const p = Array.isArray(on) ? on.shift() ?? 0 : on
  const answers: Record<string, unknown> = {}
  for (const [k, q] of Object.entries(questions)) {
    const probs = image ? looked : pick
    const [choice] = Object.entries(probs).sort((a, b) => b[1] - a[1])[0] ?? ['none']
    answers[k] = q.type === 'noul' ? { probability: image ? p : yes } : { choice, confidence: probs[choice], probabilities: probs }
  }
  return JSON.stringify({ answers, served: { model: 'decider', build: image ? build : BUILD, id: null }, usage })
}

function size(png: Buffer) {
  const { w, h, bpp, px } = unpng(png)
  let red = false
  for (let i = 0; i < px.length && !red; i += bpp) red = px[i] === 230 && px[i + 1] === 0 && px[i + 2] === 0
  return `${w}x${h}${red ? ' ring' : ''}`
}

function plan(target: string, tree: Spec[], probabilities: Record<string, number>, step: Record<string, string> = { click: target }): Promise<Result> {
  screen(tree, probabilities)
  return main({ steps: [step] })
}

function load(tree: Spec[]) {
  nodes.length = 0
  for (const [i, n] of tree.entries()) nodes.push({ bounds: [10 * i, 10, 10, 10], states: ['showing'], ...n, ref: i, depth: (nodes[n.parent]?.depth ?? -1) + 1 })
}

function lastStep(): RunStep | undefined {
  const runs = readFileSync(`${process.env.SHUTTLE_FLOWS}/.runs.jsonl`, 'utf8').trim().split('\n')
  return (JSON.parse(runs.at(-1) ?? '') as { steps: RunStep[] }).steps[0]
}

function screen(tree: Spec[], probabilities: Record<string, number>) {
  load(tree)
  pressed.length = 0
  clicked.length = 0
  asked.length = 0
  looks.length = 0
  shots = 0
  shades.length = 0
  prompts.length = 0
  pick = probabilities
}

await daemon({ after }, answer)
beforeEach(() => {
  build = null; on = 0; looked = {}; pick = {}; under = { role: 'image', name: '' }; hitPid = process.pid; yes = 0; tones = []; scale = 1
})

test('a click whose jev pick has a close runner-up escalates as ambiguous instead of pressing the pick', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Apply' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Apply to all' },
  ]
  const close = await plan('apply the settings', tree, { r1: 0.72, r2: 0.25, none: 0.03 })
  assert.equal(close.ok, false, JSON.stringify(close))
  assert.match(close.escalate?.reason ?? '', /^ambiguous target "apply the settings"/)
  assert.deepEqual(pressed, [])
  const clear = await plan('apply the settings', tree, { r1: 0.92, r2: 0.05, none: 0.03 })
  assert.equal(clear.ok, true, JSON.stringify(clear))
  assert.deepEqual(pressed, ['1.1/2'])
})

test('a click that names a link takes a link, never the row cell whose name the target contains', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'treeitem', name: 'Circuit Design Project' },
    { id: '1.1/3', parent: 1, role: 'link', name: 'View' },
    { id: '1.1/4', parent: 0, role: 'treeitem', name: 'Homework 5' },
    { id: '1.1/5', parent: 3, role: 'link', name: 'View' },
  ]
  const out = await plan('the View link in the Circuit Design Project row', tree, { r2: 0.9, r4: 0.05, none: 0.05 })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/3'])
})

test('a click takes no element that does not show, even by its exact name', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'dialog', name: 'Save' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'File type', states: [], bounds: undefined },
    { id: '1.1/3', parent: 0, role: 'combobox', name: 'Select the file type' },
  ]
  const out = await plan('File type', tree, { none: 1 })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/3'])
})

test('a menu step takes a menu bar entry that several menu bars repeat from the first of them', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Untitled 1 — LibreOffice Calc' },
    { id: '1.1/2', parent: 0, role: 'menubar', name: '' },
    { id: '1.1/3', parent: 1, role: 'menu', name: 'Format' },
    { id: '1.1/4', parent: 2, role: 'menuitem', name: 'Cells...' },
    { id: '1.1/5', parent: 0, role: 'menubar', name: '' },
    { id: '1.1/6', parent: 4, role: 'menu', name: 'Format' },
    { id: '1.1/7', parent: 5, role: 'menuitem', name: 'Cells...' },
    { id: '1.1/8', parent: 0, role: 'menubar', name: '' },
    { id: '1.1/9', parent: 7, role: 'menu', name: 'Format' },
    { id: '1.1/10', parent: 8, role: 'menuitem', name: 'Cells...' },
  ]
  const out = await plan('', tree, { r2: 0.34, r5: 0.33, r8: 0.33 }, { menu: 'Format > Cells' })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/4'])
})

test('a jev pick whose own words share none with the target, or a tab the target does not call a tab, is not found', async () => {
  const sidebar = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'FY26.fods — LibreOffice Calc' },
    { id: '1.1/2', parent: 0, role: 'panel', name: 'Navigator' },
    { id: '1.1/3', parent: 1, role: 'togglebutton', name: 'Menu' },
    { id: '1.1/4', parent: 0, role: 'button', name: 'Properties' },
  ]
  const menu = await plan('the Navigator button in the right sidebar', sidebar, { r2: 0.97, r3: 0.02, none: 0.01 })
  assert.equal(menu.ok, false, JSON.stringify(menu))
  assert.deepEqual(pressed, [])
  const browser = [
    { id: '1.2/1', parent: -1, role: 'frame', name: 'hit: house @521,277 - Google Chrome' },
    { id: '1.2/2', parent: 0, role: 'tab', name: 'hit: house @521,277 - Memory usage - 25.2 MB' },
    { id: '1.2/3', parent: 0, role: 'button', name: 'New Tab' },
  ]
  const house = await plan('the house', browser, { r1: 0.77, r2: 0.1, none: 0.13 })
  assert.equal(house.ok, false, JSON.stringify(house))
  assert.deepEqual(pressed, [])
  const plus = await plan('the green plus button at the bottom left', browser, { r2: 0.75, r1: 0.1, none: 0.15 })
  assert.equal(plus.ok, false, JSON.stringify(plus))
  assert.deepEqual(pressed, [])
})

test('a jev pick looks at the screenshot first, so of two links of the same name it takes the one the verify model places in the row', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'treeitem', name: 'Circuit Design Project' },
    { id: '1.1/3', parent: 1, role: 'link', name: 'View' },
    { id: '1.1/4', parent: 0, role: 'treeitem', name: 'Homework 5' },
    { id: '1.1/5', parent: 3, role: 'link', name: 'View' },
  ]
  build = BUILD
  looked = { r4: 0.93, r2: 0.04, none: 0.03 }
  const out = await plan('the View link for Homework 5', tree, { r2: 0.5, r4: 0.45, none: 0.05 })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/5'])
  assert.deepEqual(asked, [{ image: true }])
})

test('a jev pick on the screenshot that fails falls back to the pick on the text', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Apply' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Apply to all' },
  ]
  looked = { r2: 0.95, r1: 0.03, none: 0.02 }
  const out = await plan('apply the settings to every row', tree, { r1: 0.92, r2: 0.05, none: 0.03 })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/2'])
  assert.deepEqual(asked, [{ image: true }, { image: false }])
})

test('a grounded click is taken when jev sees the target at the point in a crop, and without that answer when jev says the element under it is the target; the run log keeps the check and its score', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = BUILD
  on = 0.92
  const seen = await plan('the yellow star', tree, {})
  assert.equal(seen.ok, true, JSON.stringify(seen))
  assert.deepEqual(clicked, ['640,400'])
  const step = lastStep()
  assert.deepEqual([step?.checked, step?.p, step?.served?.length], ['jev-image', 0.92, 1], JSON.stringify(step))
  on = 0.2
  const unseen = await plan('the yellow star', tree, {})
  assert.equal(unseen.ok, false, JSON.stringify(unseen))
  assert.deepEqual(clicked, [])
  build = null
  under = { role: 'push button', name: 'Yellow star' }
  yes = 0.9
  const checked = await plan('the yellow star', tree, {})
  assert.equal(checked.ok, true, JSON.stringify(checked))
  assert.deepEqual(clicked, ['640,400'])
  assert.deepEqual(asked, [{ image: true }, { image: false }])
  assert.ok(atPids.length > 0 && atPids.every((p) => p === win.pid), JSON.stringify(atPids))
})

test('a jev pick on the screenshot needs no word of the target in the element\'s own name, which a pick on the text does', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'dialog', name: 'Save' },
    { id: '1.1/2', parent: 0, role: 'combobox', name: 'Flat XML ODF Text Document (.fodt)' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Save' },
  ]
  build = BUILD
  looked = { r1: 0.97, r2: 0.02, none: 0.01 }
  const out = await plan('the file type dropdown', tree, { r1: 0.97, r2: 0.02, none: 0.01 })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/2'])
  build = null
  on = 0
  const textual = await plan('the file type dropdown', tree, { r1: 0.97, r2: 0.02, none: 0.01 })
  assert.equal(textual.ok, false, JSON.stringify(textual))
  assert.deepEqual(pressed, [])
})

test('computer_act\'s target keeps the jev calls that answered it as served, found or not, and none when the name alone found it', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Apply' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Apply to all' },
  ]
  const calls = (out: Result) => out.served?.map((c) => `${c.image ? 'image' : 'text'} ${c.build}`)
  build = BUILD
  looked = { r2: 0.95, r1: 0.03, none: 0.02 }
  screen(tree, {})
  const seen = await main({ locate: { target: 'apply the settings to every row' } })
  assert.equal(seen.found, true, JSON.stringify(seen))
  assert.deepEqual(calls(seen), [`image ${BUILD}`])
  build = null
  screen(tree, { none: 0.9, r1: 0.05, r2: 0.05 })
  const none = await main({ locate: { target: 'apply the settings to every row' } })
  assert.equal(none.found, false, JSON.stringify(none))
  assert.deepEqual(calls(none), ['image null', `text ${BUILD}`])
  screen(tree, {})
  const named = await main({ locate: { target: 'Apply to all' } })
  assert.equal(named.found, true, JSON.stringify(named))
  assert.equal(named.served, undefined)
})

test('locate gives a LibreOffice toolbar or tab row element the menu bar height its extents sit above it, until the frame relays out', async () => {
  const frame = (standard: number, bold: number, tab: number) => [
    { id: '1.4/1', parent: -1, role: 'window', name: 'Untitled 1 — LibreOffice Writer' },
    { id: '1.4/2', parent: 0, role: 'group', name: 'Untitled 1 — LibreOffice Writer' },
    { id: '1.4/3', parent: 1, role: 'group', name: '' },
    { id: '1.4/4', parent: 2, role: 'menubar', name: '' },
    { id: '1.4/5', parent: 3, role: 'menu', name: 'File', bounds: [0, 51, 39, 25] },
    { id: '1.4/6', parent: 1, role: 'toolbar', name: 'Standard' },
    { id: '1.4/7', parent: 5, role: 'button', name: 'Save', bounds: [92, standard, 44, 35] },
    { id: '1.4/8', parent: 1, role: 'toolbar', name: 'Formatting' },
    { id: '1.4/9', parent: 7, role: 'button', name: 'Bold', bounds: [591, bold, 33, 35] },
    { id: '1.4/10', parent: 1, role: 'tablist', name: '' },
    { id: '1.4/11', parent: 9, role: 'tab', name: 'Pipeline', bounds: [245, tab, 80, 23] },
    { id: '1.4/12', parent: 1, role: 'group', name: 'Properties' },
    { id: '1.4/13', parent: 11, role: 'button', name: 'Menu', bounds: [1234, 158, 36, 34] },
  ]
  const shift = async (tree: Spec[], target: string) => {
    load(tree)
    return (await main({ locate: { target } })).shift
  }
  assert.equal(await shift(frame(53, 92, 728), 'Bold'), 25)
  assert.equal(await shift(frame(53, 92, 728), 'Pipeline'), 25)
  assert.equal(await shift(frame(53, 92, 728), 'Menu'), 0)
  assert.equal(await shift(frame(78, 117, 753), 'Bold'), 0)
})

test('a grounded click the 640x400 crop refuses is checked on the small element\'s crop scaled up, then on the screen with a ring at the point, unless DESK_FULL_CHECK=off; the run log keeps each score and the path', async (t) => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = BUILD
  on = [0.3, 0.7]
  const zoomed = await plan('the yellow star', tree, {})
  assert.equal(zoomed.ok, true, JSON.stringify(zoomed))
  assert.deepEqual(clicked, ['640,400'])
  assert.deepEqual(looks, ['640x400', '384x288'])
  const zoomStep = lastStep()
  assert.deepEqual([zoomStep?.checked, zoomStep?.p, zoomStep?.p_zoom, zoomStep?.p_full], ['jev-image+zoom', 0.3, 0.7, undefined])
  on = [0.3, 0.6, 0.96]
  const full = await plan('the yellow star', tree, {})
  assert.equal(full.ok, true, JSON.stringify(full))
  assert.deepEqual(looks, ['640x400', '384x288', '1280x800 ring'])
  const fullStep = lastStep()
  assert.deepEqual([fullStep?.checked, fullStep?.p, fullStep?.p_zoom, fullStep?.p_full], ['jev-image+zoom+full', 0.3, 0.6, 0.96])
  on = [0.3, 0.6, 0.9]
  const refused = await plan('the yellow star', tree, {})
  assert.equal(refused.ok, false, JSON.stringify(refused))
  assert.deepEqual(clicked, [])
  under = { role: 'drawing area', name: 'Canvas', bounds: [0, 0, 800, 600] }
  on = [0.3, 0.96]
  const big = await plan('the yellow star', tree, {})
  assert.equal(big.ok, true, JSON.stringify(big))
  assert.deepEqual(looks, ['640x400', '1280x800 ring'])
  assert.equal(lastStep()?.checked, 'jev-image+full')
  under = { role: 'image', name: '' }
  process.env.DESK_FULL_CHECK = 'off'
  t.after(() => { delete process.env.DESK_FULL_CHECK })
  on = [0.3, 0.6, 0.96]
  const off = await plan('the yellow star', tree, {})
  assert.equal(off.ok, false, JSON.stringify(off))
  assert.deepEqual(looks, ['640x400', '384x288'])
  assert.equal(lastStep()?.checked, 'jev-image+zoom')
})

test('a grounded point with no accessible element under it is refused before any check, and the trace says so', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = BUILD
  on = 0.99
  under = null
  const missing = await plan('Zzqx missing control', tree, {})
  assert.equal(missing.ok, false, JSON.stringify(missing))
  assert.match(missing.escalate?.reason ?? '', /^no element matches "Zzqx missing control"; grounding pointed at no accessible element/)
  assert.deepEqual([clicked, looks], [[], []])
  assert.deepEqual(missing.trace?.[0]?.ground, { check: 'no-hit', p: undefined, p_zoom: undefined, p_full: undefined, hit: '' })
})

test('a grounded point whose element is the window root, or belongs to another process, is refused before any check', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = BUILD
  on = 0.99
  for (const role of ['frame', 'window', 'application', 'dialog']) {
    under = { role, name: 'Portal' }
    const root = await plan('Zzqx missing control', tree, {})
    assert.equal(root.ok, false, JSON.stringify(root))
    assert.match(root.escalate?.reason ?? '', new RegExp(`grounding pointed at ${role} "Portal", the window itself`))
    assert.deepEqual([clicked, looks, root.trace?.[0]?.ground?.check], [[], [], 'root-hit'])
  }
  under = { role: 'push button', name: 'Pay' }
  hitPid = process.pid + 1
  const foreign = await plan('the pay button', tree, {})
  assert.equal(foreign.ok, false, JSON.stringify(foreign))
  assert.match(foreign.escalate?.reason ?? '', /grounding pointed at push button "Pay", which belongs to another process/)
  assert.deepEqual([clicked, looks, foreign.trace?.[0]?.ground?.check], [[], [], 'foreign-hit'])
  hitPid = process.pid
  const own = await plan('the pay button', tree, {})
  assert.equal(own.ok, true, JSON.stringify(own))
  assert.deepEqual(clicked, ['640,400'])
})

test('a near-even 640x400 crop answer no longer takes a grounded click alone: the zoom and ring checks decide, and the trace carries every score and the element under the point', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = BUILD
  on = [0.56, 0.5, 0.6]
  const missing = await plan('Zzqx missing control', tree, {})
  assert.equal(missing.ok, false, JSON.stringify(missing))
  assert.deepEqual(clicked, [])
  assert.deepEqual(looks, ['640x400', '384x288', '1280x800 ring'])
  assert.deepEqual(missing.trace?.[0]?.ground, { check: 'jev-image+zoom+full', p: 0.56, p_zoom: 0.5, p_full: 0.6, hit: 'image' })
  under = { role: 'push button', name: 'Open Parent', bounds: [700, 380, 40, 40] }
  on = [0.3, 0.74]
  const real = await plan('the up arrow icon in the toolbar', tree, {})
  assert.equal(real.ok, true, JSON.stringify(real))
  assert.deepEqual(clicked, ['640,400'])
  assert.deepEqual(real.trace?.[0], { i: 0, op: 'click', how: 'ground', ms: real.trace?.[0]?.ms, ground: { check: 'jev-image+zoom', p: 0.3, p_zoom: 0.74, p_full: undefined, hit: 'push button "Open Parent"' } })
})

test('a grounded step checks fresh captures taken after grounding: the crop, then one screen capture that the zoom and ring are cut from', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = BUILD
  on = [0.3, 0.6, 0.96]
  tones = [60, 200, 200]
  const full = await plan('the yellow star', tree, {})
  assert.equal(full.ok, true, JSON.stringify(full))
  assert.deepEqual(looks, ['640x400', '384x288', '1280x800 ring'])
  assert.deepEqual([shots, shades], [3, [200, 200, 200]])
  on = 0.92
  tones = [60, 200]
  await plan('the yellow star', tree, {})
  assert.deepEqual([shots, shades], [2, [200]])
})

test('a grounded check places the point in capture pixels on a 2x capture', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = BUILD
  on = [0.3, 0.6, 0.96]
  scale = 2
  const retina = await plan('the yellow star', tree, {})
  assert.equal(retina.ok, true, JSON.stringify(retina))
  assert.deepEqual(looks, ['1280x800', '384x288', '2560x1600 ring'])
  assert.match(prompts[0] ?? '', /pixel \(640, 400\)/)
  assert.match(prompts[2] ?? '', /2560x1600.*pixel \(1280, 800\)/)
})

test('a click whose target is a name plus role words takes the showing button of that name over hidden menu entries, and a hidden menu entry when nothing showing matches', async () => {
  const tree = [
    { id: '1.5/1', parent: -1, role: 'window', name: 'Untitled 1 — LibreOffice Writer' },
    { id: '1.5/2', parent: 0, role: 'menubar', name: '' },
    { id: '1.5/3', parent: 1, role: 'menu', name: 'Format', bounds: [0, 51, 60, 25] },
    { id: '1.5/4', parent: 2, role: 'menuitemcheckbox', name: 'Bold', states: [], bounds: undefined },
    { id: '1.5/5', parent: 2, role: 'menuitem', name: 'Save As...', states: [], bounds: undefined },
    { id: '1.5/6', parent: 0, role: 'toolbar', name: 'Formatting' },
    { id: '1.5/7', parent: 5, role: 'button', name: 'Bold', bounds: [591, 92, 33, 35] },
  ]
  const bold = await plan('the Bold button', tree, { r6: 0.75, none: 0.25 })
  assert.equal(bold.ok, true, JSON.stringify(bold))
  assert.deepEqual(pressed, ['1.5/7'])
  const saveAs = await plan('Save As', tree, { none: 1 })
  assert.equal(saveAs.ok, true, JSON.stringify(saveAs))
  assert.deepEqual(pressed, ['1.5/5'])
})

test('an unnamed control takes its own description, else the name, else the description, of the ancestors that hold only it, so LibreOffice\'s sidebar tab buttons read as their decks', async () => {
  const sidebar = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Donor_Report.fodt — LibreOffice Writer' },
    { id: '1.1/2', parent: 0, role: 'button', name: '', description: 'Close Document' },
    { id: '1.1/3', parent: 0, role: 'group', name: 'Properties' },
    { id: '1.1/4', parent: 2, role: 'button', name: 'Menu', description: 'Sidebar Settings' },
    { id: '1.1/5', parent: 2, role: 'toolbar', name: 'Styles', description: 'Styles' },
    { id: '1.1/6', parent: 4, role: 'group', name: '', description: 'Styles (Alt+2)' },
    { id: '1.1/7', parent: 5, role: 'button', name: '' },
    { id: '1.1/8', parent: 2, role: 'toolbar', name: 'Navigator', description: 'Navigator' },
    { id: '1.1/9', parent: 7, role: 'group', name: '', description: 'Navigator (Alt+4)' },
    { id: '1.1/10', parent: 8, role: 'button', name: '' },
    { id: '1.1/11', parent: 0, role: 'toolbar', name: '' },
    { id: '1.1/12', parent: 10, role: 'group', name: '', description: 'Close Infobar' },
    { id: '1.1/13', parent: 11, role: 'button', name: '' },
  ]
  const named = await plan('Navigator', sidebar, { none: 1 })
  assert.equal(named.ok, true, JSON.stringify(named))
  assert.equal(named.trace?.[0]?.how, 'ax')
  assert.deepEqual(pressed, ['1.1/10'])
  const picked = await plan('the Navigator button in the sidebar', sidebar, { r9: 0.96, r3: 0.03, none: 0.01 })
  assert.equal(picked.ok, true, JSON.stringify(picked))
  assert.deepEqual(pressed, ['1.1/10'])
  for (const [target, id] of [['Close Document', '1.1/2'], ['Close Infobar', '1.1/13']] as const) {
    const out = await plan(target, sidebar, { none: 1 })
    assert.equal(out.ok, true, JSON.stringify(out))
    assert.deepEqual(pressed, [id])
  }
})

test('a jev pick of an element whose role the target\'s own word rules out, a spin button for "the plus button", is not found; a radio button is still one', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'dialog', name: 'Format Cells' },
    { id: '1.1/2', parent: 0, role: 'spinbutton', name: 'Decimal places:' },
    { id: '1.1/3', parent: 0, role: 'spinbutton', name: 'Leading zeroes:' },
    { id: '1.1/4', parent: 0, role: 'radio', name: 'Percent' },
    { id: '1.1/5', parent: 0, role: 'button', name: 'OK' },
  ]
  build = BUILD
  looked = { r1: 0.86, r2: 0.05, none: 0.09 }
  on = 0
  const plus = await plan('the plus button that increases the decimal places', tree, { r1: 0.86, none: 0.14 })
  assert.equal(plus.ok, false, JSON.stringify(plus))
  assert.deepEqual(pressed, [])
  assert.deepEqual(clicked, [])
  looked = { r3: 0.95, r4: 0.03, none: 0.02 }
  const radio = await plan('the Percent radio button', tree, { r3: 0.95, none: 0.05 })
  assert.equal(radio.ok, true, JSON.stringify(radio))
  assert.deepEqual(pressed, ['1.1/4'])
})

test('a target the focused window lacks resolves in the panel or desktop window when it names the dock, panel, taskbar or desktop, and only then', { skip: process.platform !== 'linux' }, async (t) => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'dialog', name: 'LibreOffice 24.2 Document Recovery' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Start' },
  ]
  t.after(() => { shell.length = 0 })
  shell.push({
    win: { id: '1.5/2', title: '', role: 'window', pid: 533, active: false },
    nodes: [
      { ref: 0, id: '1.5/20', parent: -1, depth: 0, role: 'window', name: '', states: ['showing'], bounds: [0, 752, 1280, 48] },
      { ref: 1, id: '1.5/21', parent: 0, depth: 1, role: 'button', name: 'File Manager', states: ['showing'], bounds: [592, 752, 48, 48] },
      { ref: 2, id: '1.5/22', parent: 0, depth: 1, role: 'togglebutton', name: 'Directory Menu', states: ['showing'], bounds: [744, 752, 48, 48] },
    ],
  })
  screen(tree, {})
  const dock = await main({ locate: { target: 'the File Manager launcher in the bottom dock', press: true } })
  assert.equal(dock.found, true, JSON.stringify(dock))
  assert.deepEqual([dock.name, dock.window, dock.pid, dock.bounds], ['File Manager', '', 533, [592, 752, 48, 48]])
  assert.deepEqual(pressed, ['1.5/21'])
  screen(tree, {})
  const unnamed = await main({ locate: { target: 'the File Manager launcher', press: true } })
  assert.equal(unnamed.found, false, JSON.stringify(unnamed))
  assert.deepEqual(pressed, [])
})

test('an item of a list or tree grid is a target: a drawn layout tile wins over the hidden menu entries of the same name', async () => {
  const tree = [
    { id: '1.4/1', parent: -1, role: 'window', name: 'Deck — LibreOffice Impress' },
    { id: '1.4/2', parent: 0, role: 'menu', name: 'Layout', states: [] },
    { id: '1.4/3', parent: 1, role: 'menuitem', name: 'Title Only', states: [] },
    { id: '1.4/4', parent: 0, role: 'list', name: 'Layouts' },
    { id: '1.4/5', parent: 3, role: 'listitem', name: 'Title, Content' },
    { id: '1.4/6', parent: 3, role: 'listitem', name: 'Title Only' },
    { id: '1.4/7', parent: 0, role: 'treegrid', name: '' },
    { id: '1.4/8', parent: 6, role: 'cell', name: 'Percent' },
    { id: '1.4/9', parent: 0, role: 'cell', name: 'Currency' },
  ]
  const bounds = (id: string) => [10 * tree.findIndex((n) => n.id === id), 10, 10, 10]
  for (const [target, id, how] of [['the Title Only layout', '1.4/6', 'ax'], ['the Title, Content layout in the Layouts panel', '1.4/5', 'ax'], ['Percent in the Category list', '1.4/8', 'jev(0.95)']] as const) {
    screen(tree, { r7: 0.95, none: 0.05 })
    const out = await main({ locate: { target } })
    assert.deepEqual([out.found, out.how, out.bounds], [true, how, bounds(id)], `${target}: ${JSON.stringify(out)}`)
  }
  screen(tree, { r8: 0.95, none: 0.05 })
  const loose = await main({ locate: { target: 'the Currency entry' } })
  assert.equal(loose.found, false, JSON.stringify(loose))
})
