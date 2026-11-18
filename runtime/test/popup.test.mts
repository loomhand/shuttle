import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main as plan } from '../lib/gui_plan.mts'

const BASE = 'an untitled dialog of "R89Kit" is in front of "r89aN3" and takes all of its app\'s input until it closes: answer it with {click: "<its button>"} and no window, or close it with {close: true}; nothing was sent'
const NAMED = 'an untitled dialog of "R89Kit" is in front of "r89aN3" and takes all of its app\'s input until it closes, and a step that names "r89aN3" never acts in the dialog: answer it with {click: "<its button>"} and no window, or close it with {close: true}; nothing was sent'
const INPUT = 'an untitled dialog of "R89Kit" is in front of "r89aN3" and takes all of its app\'s input until it closes, and a type or key would go into it: answer it with {click: "<its button>"} and no window, or close it with {close: true}; nothing was sent'
const BOTH = '"Delete" is in both the untitled dialog of "R89Kit" and "r89aN3" behind it, and the step never guesses which one it means: answer the dialog with a button only it has, or close it with {close: true}, then click "Delete"; nothing was sent'

test('a click that opens a combo box\'s list leaves the next step in the dialog, whose tree holds the list\'s entries, also in a new plan', async (t) => {
  const wm = { id: '1.3/1', title: '', role: 'window', pid: 687, active: true }
  const main = { id: '1.14/117', title: 'Untitled 1 - Mousepad', role: 'window', pid: 889, active: false }
  const prefs = { id: '1.14/322', title: 'Mousepad Preferences', role: 'dialog', pid: 889, active: true }
  const list = { id: '1.14/537', title: '', role: 'window', pid: 889, active: true }
  let open = false
  const nodes = [
    { ref: 0, id: '1.14/322', parent: -1, depth: 0, role: 'dialog', name: 'Mousepad Preferences', states: ['showing'] },
    { ref: 1, id: '1.14/400', parent: 0, depth: 1, role: 'combobox', name: 'Insert Tabs', states: ['showing'], bounds: [420, 300, 160, 30] },
    { ref: 2, id: '1.14/401', parent: 1, depth: 2, role: 'menu', name: '', states: [] },
    { ref: 3, id: '1.14/402', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Tabs', states: [], bounds: [420, 300, 160, 26] },
    { ref: 4, id: '1.14/403', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Spaces', states: [], bounds: [420, 326, 160, 26] },
  ]
  const pressed: string[] = []
  const snapped: string[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(open ? [wm, list, main, { ...prefs, active: false }] : [wm, main, prefs])
    if (tool === 'desk_snapshot') {
      snapped.push(String(args.window))
      const shown = nodes.map((n) => ({ ...n, states: n.depth === 3 && open ? ['showing'] : n.states }))
      return JSON.stringify({ window: prefs, nodes: args.window === prefs.title ? shown : [], truncated: false })
    }
    if (tool === 'desk_press') { pressed.push(String(args.id)); open = args.id === '1.14/400'; return '{"ok":true,"how":"click"}' }
    return '{"ok":true}'
  })
  const both = await plan({ steps: [{ click: 'Insert Tabs' }, { click: 'Insert Spaces' }] })
  assert.equal(both.ok, true, JSON.stringify(both))
  assert.deepEqual(pressed, ['1.14/400', '1.14/403'])
  assert.equal(both.window, 'Mousepad Preferences')
  const left = await plan({ steps: [{ click: 'Insert Tabs' }] })
  assert.equal(left.ok, true, JSON.stringify(left))
  const next = await plan({ steps: [{ click: 'Insert Spaces' }] })
  assert.equal(next.ok, true, JSON.stringify(next))
  assert.deepEqual(pressed.slice(2), ['1.14/400', '1.14/403'])
  assert.ok(snapped.every((w) => w === 'Mousepad Preferences'), snapped.join(' '))
})

test('a macOS click step with an untitled app-modal alert of the app in front stops before it acts behind the alert, and the plan is not saved', { skip: process.platform !== 'darwin' }, async (t) => {
  const doc = { id: '198', title: 'r89aN3', role: 'window', pid: 1966, active: true, app: 'R89Kit', bounds: [100, 120, 520, 400] }
  const alert = { id: '201', title: '', role: 'dialog', pid: 1966, active: true, app: 'R89Kit', bounds: [510, 155, 260, 208] }
  const nodes = [
    { ref: 0, id: '198/0', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] },
    { ref: 1, id: '198/1', parent: 0, depth: 1, role: 'button', name: 'Alert now', states: ['showing'], bounds: [120, 200, 80, 30] },
    { ref: 2, id: '198/2', parent: 0, depth: 1, role: 'button', name: 'Tool', states: ['showing'], bounds: [560, 140, 40, 30] },
  ]
  const shown = [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }, { ref: 1, id: '201/1', parent: 0, depth: 1, role: 'button', name: 'Close', states: ['showing'] }]
  let up = false
  const pressed: string[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(!up ? [doc] : pressed.includes('198/2') ? [doc, { ...alert, active: false }] : [{ ...doc, active: false }, alert])
    if (tool === 'desk_snapshot') return JSON.stringify(args.window === doc.title ? { window: doc, nodes, truncated: false } : { window: alert, nodes: shown, truncated: false })
    if (tool === 'desk_press') { pressed.push(String(args.id)); up ||= args.id === '198/1'; return '{"ok":true,"how":"ax"}' }
    return '{"ok":true}'
  })
  const r = await plan({ steps: [{ app: 'R89Kit' }, { click: 'Alert now' }, { click: 'Tool' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.saved, undefined, JSON.stringify(r))
  assert.equal(r.escalate?.step, 2, JSON.stringify(r))
  assert.equal(r.escalate?.reason, BASE)
  assert.deepEqual(pressed, ['198/1'])
})

test('a macOS alert of the app open before the plan stops a step meant for the window behind it, one that names that window, a type or key, and a click whose name both have, each with nothing sent, and lets a click on a button only it has through', { skip: process.platform !== 'darwin' }, async (t) => {
  for (const [step, want, risky] of [[{ click: 'Tool' }, BASE, 'Delete'], [{ click: 'Tool', window: 'r89aN3' }, NAMED, 'Delete'], [{ type: 'x' }, INPUT, 'Delete'], [{ key: 'return' }, INPUT, 'Delete'], [{ click: 'OK' }, '', 'Delete'], [{ click: 'Delete', window: 'r89aN3' }, NAMED, 'Delete'], [{ click: 'OK', window: 'r89aN3' }, NAMED, 'Delete'], [{ click: 'Delete' }, BOTH, 'Delete'], [{ click: 'Delete' }, BOTH, 'Delete…']] as const) {
    const doc = { id: '198', title: 'r89aN3', role: 'window', pid: 1966, active: true, app: 'R89Kit' }
    const alert = { id: '201', title: '', role: 'dialog', pid: 1966, active: false, app: 'R89Kit' }
    const nodes = [{ ref: 0, id: '198/0', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }, { ref: 1, id: '198/2', parent: 0, depth: 1, role: 'button', name: 'Tool', states: ['showing'] }, { ref: 2, id: '198/3', parent: 0, depth: 1, role: 'button', name: 'Delete', states: ['showing'] }]
    const own = [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }, { ref: 1, id: '201/1', parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'] }, { ref: 2, id: '201/2', parent: 0, depth: 1, role: 'button', name: risky, states: ['showing'] }]
    let open = true; const input: string[] = []
    await daemon(t, ({ tool, args }) => {
      if (tool === 'desk_windows') return JSON.stringify(open ? [doc, alert] : [doc])
      if (tool === 'desk_snapshot') return JSON.stringify(args.window === '' || args.id === alert.id ? { window: alert, nodes: own, truncated: false } : { window: doc, nodes, truncated: false })
      if (tool === 'desk_press') { input.push(String(args.id)); open &&= args.id !== '201/1'; return '{"ok":true,"how":"action"}' }
      if (tool === 'desk_type' || tool === 'desk_key' || tool === 'desk_click') input.push(tool)
      return '{"ok":true}'
    })
    const r = await plan({ steps: [{ app: 'R89Kit' }, step] })
    if (!want) { assert.equal(r.ok, true, JSON.stringify(r)); assert.deepEqual(input, ['201/1']); continue }
    assert.equal(r.escalate?.step, 1, `${JSON.stringify(step)} ${JSON.stringify(r)}`)
    assert.equal(r.escalate?.reason, want, JSON.stringify(step))
    assert.deepEqual(input, [], JSON.stringify(step))
  }
})
