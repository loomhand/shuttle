import assert from 'node:assert/strict'
import { mkdtempSync, readFileSync } from 'node:fs'
import { createServer as http } from 'node:http'
import { createServer } from 'node:net'
import { homedir, tmpdir } from 'node:os'
import { after, before, test } from 'node:test'
import { crc32, deflateSync, inflateSync } from 'node:zlib'

type El = { ref: number; id: string; parent: number; depth: number; role: string; name: string; states: string[]; bounds?: number[] }
type Spec = Omit<El, 'ref' | 'depth' | 'states'> & { states?: string[] }

const win = { id: '1.1/1', title: 'Portal', role: 'frame', pid: process.pid, active: true }
const nodes: El[] = []
const pressed: string[] = []
const clicked: string[] = []
const asked: { model: string; image: boolean }[] = []
const looks: string[] = []
let pick: Record<string, number> = {}
let looked: Record<string, number> = {}
let build: string | null = null
let text = 'typesafe/jev-1.14-20261014'
let on: number | number[] = 0
let under: { role: string; name: string; bounds?: number[] } | null = null
let yes = 0

const toolhost = createServer((s) => {
  let buf = Buffer.alloc(0)
  s.on('data', (d) => {
    buf = Buffer.concat([buf, d])
    if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
    const { tool, args } = JSON.parse(buf.subarray(4).toString())
    let text = '{"ok":true}'
    if (tool === 'desk_windows') text = JSON.stringify([win])
    if (tool === 'desk_snapshot') text = JSON.stringify({ window: win, nodes, truncated: false, text: '- frame "Portal"' })
    if (tool === 'desk_press') { pressed.push(args.id); text = '{"ok":true,"how":"click"}' }
    if (tool === 'desk_shot') text = JSON.stringify({ png_b64: gray(args.w ?? 1280, args.h ?? 800).toString('base64') })
    if (tool === 'desk_screen') text = '{"width":1280,"height":800}'
    if (tool === 'desk_at') text = JSON.stringify({ found: under, path: [] })
    if (tool === 'desk_click') clicked.push(`${args.x},${args.y}`)
    const body = Buffer.from(JSON.stringify({ text, is_error: false }))
    const head = Buffer.alloc(4)
    head.writeUInt32BE(body.length)
    s.end(Buffer.concat([head, body]))
  })
})
const gw = http((req, res) => {
  let body = ''
  req.on('data', (c) => { body += c })
  req.on('end', () => {
    const { model, state, questions } = JSON.parse(body)
    if (req.url?.endsWith('/chat/completions')) return res.end(JSON.stringify({ choices: [{ message: { content: '{"point_2d": [500, 500]}' } }] }))
    const image = JSON.stringify(state).includes('"image_url"')
    asked.push({ model, image })
    if (image && !build) { res.statusCode = 500; return res.end('{}') }
    if (image) looks.push(state.filter((p: { image_url?: { url: string } }) => p.image_url).map((p: { image_url: { url: string } }) => size(Buffer.from(p.image_url.url.split(',')[1], 'base64'))).join(' '))
    const p = Array.isArray(on) ? on.shift() ?? 0 : on
    const answers: Record<string, unknown> = {}
    for (const [k, q] of Object.entries(questions) as [string, { type: string }][]) {
      const probs = image ? looked : pick
      const [choice] = Object.entries(probs).sort((a, b) => b[1] - a[1])[0] ?? ['none']
      answers[k] = q.type === 'noul' ? { type: 'noul', noul: image ? p : yes } : { type: 'choice', choice, confidence: probs[choice], probabilities: probs }
    }
    res.end(JSON.stringify({ model: image ? build : text, answers }))
  })
})

// an RGBA PNG of one grey, and the size of a PNG with " ring" when it holds the red of the drawn ring
function gray(w: number, h: number) {
  const chunk = (kind: string, data: Buffer) => {
    const head = Buffer.alloc(8), crc = Buffer.alloc(4); head.writeUInt32BE(data.length); head.write(kind, 4, 'ascii')
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data])) >>> 0); return Buffer.concat([head, data, crc])
  }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 6
  const raw = Buffer.alloc(h * (w * 4 + 1), 128); for (let y = 0; y < h; y++) raw[y * (w * 4 + 1)] = 0
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))])
}
function size(png: Buffer) {
  const [w, h] = [png.readUInt32BE(16), png.readUInt32BE(20)]
  const raw = inflateSync(png.subarray(41, 41 + png.readUInt32BE(33)))
  let red = false
  for (let y = 0; y < h && !red; y++) for (let x = 0; x < w && !red; x++) { const i = y * (w * 4 + 1) + 1 + x * 4; red = raw[i] === 230 && raw[i + 1] === 0 && raw[i + 2] === 0 }
  return `${w}x${h}${red ? ' ring' : ''}`
}

async function plan(target: string, tree: Spec[], probabilities: Record<string, number>, step: Record<string, string> = { click: target }) {
  const { main } = await screen(tree, probabilities)
  return main({ steps: [step] })
}

async function screen(tree: Spec[], probabilities: Record<string, number>) {
  nodes.length = 0
  for (const [i, n] of tree.entries()) nodes.push({ bounds: [10 * i, 10, 10, 10], states: ['showing'], ...n, ref: i, depth: n.parent < 0 ? 0 : nodes[n.parent].depth + 1 })
  pressed.length = 0
  clicked.length = 0
  asked.length = 0
  looks.length = 0
  pick = probabilities
  return import('../rootfs/opt/desk/lib/gui_plan.mts')
}

before(async () => {
  const socket = `${mkdtempSync(`${tmpdir()}/gui-plan-`)}/toolhost.sock`
  await new Promise<void>((r) => toolhost.listen(socket, r))
  await new Promise<void>((r) => gw.listen(0, '127.0.0.1', r))
  process.env.TOOLHOST_SOCKET = socket
  process.env.GW_BASE_URL = `http://127.0.0.1:${(gw.address() as { port: number }).port}`
})
after(() => {
  toolhost.close()
  gw.close()
})

test('a click whose jev pick has a close runner-up escalates as ambiguous instead of pressing the pick', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Apply' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Apply to all' },
  ]
  const close = await plan('apply the settings', tree, { r1: 0.72, r2: 0.25, none: 0.03 })
  assert.equal(close.ok, false, JSON.stringify(close))
  assert.match(close.escalate.reason, /^ambiguous target "apply the settings"/)
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

test('a jev pick looks at the screenshot first, so of two links of the same name it takes the one jev-1.14 places in the row', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'treeitem', name: 'Circuit Design Project' },
    { id: '1.1/3', parent: 1, role: 'link', name: 'View' },
    { id: '1.1/4', parent: 0, role: 'treeitem', name: 'Homework 5' },
    { id: '1.1/5', parent: 3, role: 'link', name: 'View' },
  ]
  build = 'typesafe/jev-1.14-20261014'
  looked = { r4: 0.93, r2: 0.04, none: 0.03 }
  const out = await plan('the View link for Homework 5', tree, { r2: 0.5, r4: 0.45, none: 0.05 })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/5'])
  assert.deepEqual(asked, [{ model: 'typesafe/jev-1.14', image: true }])
  build = null
})

test('a jev pick on the screenshot that another version or an alias answers, that fails or that gets no reply falls back to the pick on the text, which only a jev-1.14 build may answer', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Apply' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Apply to all' },
  ]
  looked = { r2: 0.95, r1: 0.03, none: 0.02 }
  for (const served of ['typesafe/jev-1.13-20260917', '~typesafe/jev-latest', null]) {
    build = served
    const out = await plan('apply the settings to every row', tree, { r1: 0.92, r2: 0.05, none: 0.03 })
    assert.equal(out.ok, true, JSON.stringify(out))
    assert.deepEqual(pressed, ['1.1/2'], String(served))
    assert.deepEqual(asked, [{ model: 'typesafe/jev-1.14', image: true }, { model: 'typesafe/jev-1.14', image: false }])
  }
  text = 'typesafe/jev-1.13-20260917'
  const older = await plan('apply the settings to every row', tree, { r1: 0.92, r2: 0.05, none: 0.03 })
  assert.equal(older.ok, false, JSON.stringify(older))
  assert.deepEqual(pressed, [])
  assert.match(older.escalate.reason, /served typesafe\/jev-1.13-20260917 for typesafe\/jev-1.14/)
  text = 'typesafe/jev-1.14-20261014'
  build = null
})

test('a jev-1.14 build other than the tested one answers, each call keeps its build in served, and stderr says so once', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Apply' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Apply to all' },
  ]
  const desk = await import('../rootfs/opt/desk/lib/desk.mts')
  const lines: string[] = []
  const write = process.stderr.write
  process.stderr.write = ((s: string | Uint8Array) => { lines.push(String(s)); return true }) as typeof process.stderr.write
  build = text = 'typesafe/jev-1.14-20991231'
  looked = { r2: 0.95, r1: 0.03, none: 0.02 }
  try {
    for (let i = 0; i < 2; i++) {
      const out = await plan('apply the settings to every row', tree, { r1: 0.92, r2: 0.05, none: 0.03 })
      assert.equal(out.ok, true, JSON.stringify(out))
      assert.deepEqual(pressed, ['1.1/3'])
    }
  } finally { process.stderr.write = write }
  assert.deepEqual(desk.served.slice(-2).map((c: { build: string | null }) => c.build), ['typesafe/jev-1.14-20991231', 'typesafe/jev-1.14-20991231'])
  assert.equal(lines.filter((l) => l.includes('jev answers from build typesafe/jev-1.14-20991231')).length, 1, lines.join(''))
  build = null
  text = 'typesafe/jev-1.14-20261014'
})

test('a grounded click is taken when jev sees the target at the point in a crop, and without that answer when jev says the element under it is the target; the run log keeps the check and its score', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  build = 'typesafe/jev-1.14-20261014'
  on = 0.8
  const seen = await plan('the yellow star', tree, {})
  assert.equal(seen.ok, true, JSON.stringify(seen))
  assert.deepEqual(clicked, ['640,400'])
  const runs = readFileSync(`${process.platform === 'linux' ? '/workspace' : `${homedir()}/workspace`}/flows/.runs.jsonl`, 'utf8').trim().split('\n')
  const step = JSON.parse(runs.at(-1)!).steps[0]
  assert.deepEqual([step.checked, step.p, step.served.length], ['jev-image', 0.8, 1], JSON.stringify(step))
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
  assert.deepEqual(asked, [{ model: 'typesafe/jev-1.14', image: true }, { model: 'typesafe/jev-1.14', image: false }])
  under = null
})

test('a jev-1.14 pick on the screenshot needs no word of the target in the element\'s own name, which a pick on the text does', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'dialog', name: 'Save' },
    { id: '1.1/2', parent: 0, role: 'combobox', name: 'Flat XML ODF Text Document (.fodt)' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Save' },
  ]
  build = 'typesafe/jev-1.14-20261014'
  looked = { r1: 0.97, r2: 0.02, none: 0.01 }
  const out = await plan('the file type dropdown', tree, { r1: 0.97, r2: 0.02, none: 0.01 })
  assert.equal(out.ok, true, JSON.stringify(out))
  assert.deepEqual(pressed, ['1.1/2'])
  build = null
  on = 0
  const text = await plan('the file type dropdown', tree, { r1: 0.97, r2: 0.02, none: 0.01 })
  assert.equal(text.ok, false, JSON.stringify(text))
  assert.deepEqual(pressed, [])
})

test('computer_act\'s target keeps the jev calls that answered it as served, found or not, and none when the name alone found it', async () => {
  const tree = [
    { id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' },
    { id: '1.1/2', parent: 0, role: 'button', name: 'Apply' },
    { id: '1.1/3', parent: 0, role: 'button', name: 'Apply to all' },
  ]
  const calls = (out: { served?: { image: boolean; build: string | null }[] }) => out.served?.map((c) => `${c.image ? 'image' : 'text'} ${c.build}`)
  build = 'typesafe/jev-1.14-20261014'
  looked = { r2: 0.95, r1: 0.03, none: 0.02 }
  const seen = await (await screen(tree, {})).main({ locate: { target: 'apply the settings to every row' } })
  assert.equal(seen.found, true, JSON.stringify(seen))
  assert.deepEqual(calls(seen), ['image typesafe/jev-1.14-20261014'])
  build = null
  const none = await (await screen(tree, { none: 0.9, r1: 0.05, r2: 0.05 })).main({ locate: { target: 'apply the settings to every row' } })
  assert.equal(none.found, false, JSON.stringify(none))
  assert.deepEqual(calls(none), ['image null', 'text typesafe/jev-1.14-20261014'])
  const named = await (await screen(tree, {})).main({ locate: { target: 'Apply to all' } })
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
    nodes.length = 0
    for (const [i, n] of tree.entries()) nodes.push({ bounds: [10 * i, 10, 10, 10], states: ['showing'], ...n, ref: i, depth: n.parent < 0 ? 0 : nodes[n.parent].depth + 1 })
    const { main } = await import('../rootfs/opt/desk/lib/gui_plan.mts')
    return (await main({ locate: { target } } as never) as { shift?: number | null }).shift
  }
  assert.equal(await shift(frame(53, 92, 728), 'Bold'), 25)
  assert.equal(await shift(frame(53, 92, 728), 'Pipeline'), 25)
  assert.equal(await shift(frame(53, 92, 728), 'Menu'), 0)
  assert.equal(await shift(frame(78, 117, 753), 'Bold'), 0)
})

test('a grounded click the 640x400 crop refuses is checked on the small element\'s crop scaled up, then on the screen with a ring at the point, unless DESK_FULL_CHECK=off; the run log keeps each score and the path', async () => {
  const tree = [{ id: '1.1/1', parent: -1, role: 'frame', name: 'Portal' }]
  const step = () => JSON.parse(readFileSync(`${process.platform === 'linux' ? '/workspace' : `${homedir()}/workspace`}/flows/.runs.jsonl`, 'utf8').trim().split('\n').at(-1)!).steps[0]
  build = 'typesafe/jev-1.14-20261014'
  on = [0.3, 0.7]
  const zoomed = await plan('the yellow star', tree, {})
  assert.equal(zoomed.ok, true, JSON.stringify(zoomed))
  assert.deepEqual(clicked, ['640,400'])
  assert.deepEqual(looks, ['640x400', '384x288'])
  assert.deepEqual([step().checked, step().p, step().p_zoom, step().p_full], ['jev-image+zoom', 0.3, 0.7, undefined])
  on = [0.3, 0.6, 0.96]
  const full = await plan('the yellow star', tree, {})
  assert.equal(full.ok, true, JSON.stringify(full))
  assert.deepEqual(looks, ['640x400', '384x288', '1280x800 ring'])
  assert.deepEqual([step().checked, step().p, step().p_zoom, step().p_full], ['jev-image+zoom+full', 0.3, 0.6, 0.96])
  on = [0.3, 0.6, 0.9]
  const refused = await plan('the yellow star', tree, {})
  assert.equal(refused.ok, false, JSON.stringify(refused))
  assert.deepEqual(clicked, [])
  under = { role: 'drawing area', name: 'Canvas', bounds: [0, 0, 800, 600] }
  on = [0.3, 0.96]
  const big = await plan('the yellow star', tree, {})
  assert.equal(big.ok, true, JSON.stringify(big))
  assert.deepEqual(looks, ['640x400', '1280x800 ring'])
  assert.equal(step().checked, 'jev-image+full')
  under = null
  process.env.DESK_FULL_CHECK = 'off'
  on = [0.3, 0.6, 0.96]
  const off = await plan('the yellow star', tree, {})
  delete process.env.DESK_FULL_CHECK
  assert.equal(off.ok, false, JSON.stringify(off))
  assert.deepEqual(looks, ['640x400', '384x288'])
  assert.equal(step().checked, 'jev-image+zoom')
  build = null
  on = 0
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
  assert.equal(named.trace[0].how, 'ax')
  assert.deepEqual(pressed, ['1.1/10'])
  const picked = await plan('the Navigator button in the sidebar', sidebar, { r9: 0.96, r3: 0.03, none: 0.01 })
  assert.equal(picked.ok, true, JSON.stringify(picked))
  assert.deepEqual(pressed, ['1.1/10'])
  for (const [target, id] of [['Close Document', '1.1/2'], ['Close Infobar', '1.1/13']]) {
    const out = await plan(target, sidebar, { none: 1 })
    assert.equal(out.ok, true, JSON.stringify(out))
    assert.deepEqual(pressed, [id])
  }
})
