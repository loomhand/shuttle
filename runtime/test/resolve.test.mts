import assert from 'node:assert/strict'
import { mkdtempSync } from 'node:fs'
import { createServer as http } from 'node:http'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { after, before, test } from 'node:test'

type El = { ref: number; id: string; parent: number; depth: number; role: string; name: string; states: string[]; bounds?: number[] }
type Spec = Omit<El, 'ref' | 'depth' | 'states'> & { states?: string[] }

const win = { id: '1.1/1', title: 'Portal', role: 'frame', pid: process.pid, active: true }
const nodes: El[] = []
const pressed: string[] = []
let pick: Record<string, number> = {}

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
    const { questions } = JSON.parse(body)
    const answers: Record<string, unknown> = {}
    for (const k of Object.keys(questions)) {
      const [choice] = Object.entries(pick).sort((a, b) => b[1] - a[1])[0]
      answers[k] = { type: 'choice', choice, confidence: pick[choice], probabilities: pick }
    }
    res.end(JSON.stringify({ answers }))
  })
})

async function plan(target: string, tree: Spec[], probabilities: Record<string, number>, step: Record<string, string> = { click: target }) {
  nodes.length = 0
  for (const [i, n] of tree.entries()) nodes.push({ bounds: [10 * i, 10, 10, 10], states: ['showing'], ...n, ref: i, depth: n.parent < 0 ? 0 : nodes[n.parent].depth + 1 })
  pressed.length = 0
  pick = probabilities
  const { main } = await import('../rootfs/opt/desk/lib/gui_plan.mts')
  return main({ steps: [step] })
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
  const close = await plan('confirm the settings', tree, { r1: 0.72, r2: 0.25, none: 0.03 })
  assert.equal(close.ok, false, JSON.stringify(close))
  assert.match(close.escalate.reason, /^ambiguous target "confirm the settings"/)
  assert.deepEqual(pressed, [])
  const clear = await plan('confirm the settings', tree, { r1: 0.92, r2: 0.05, none: 0.03 })
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
