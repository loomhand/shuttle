import assert from 'node:assert/strict'
import { readFileSync, rmSync, writeFileSync } from 'node:fs'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main as plan } from '../lib/gui_plan.mts'

const TEXT = 'an untitled dialog of "R89Kit" is in front of "r89aN3" and takes all of its app\'s input until it closes, and a plan never presses a button of such a dialog: close it with {close: true}, or press its button with the desk tool (snapshot with the dialog\'s id from desk windows, then press with the button\'s id); nothing was sent'
const ALONE = 'an untitled dialog of "R89Kit" takes all of its app\'s input until it closes, and a plan never presses a button of such a dialog: close it with {close: true}, or press its button with the desk tool (snapshot with the dialog\'s id from desk windows, then press with the button\'s id); nothing was sent'
const SEVERAL = '2 untitled dialogs of "R89Kit" are open, and a plan never picks one of them: press the button meant with the desk tool (snapshot with that dialog\'s id from desk windows, then press with the button\'s id); nothing was sent'

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
    if (tool === 'desk_windows') return JSON.stringify(!up ? [doc] : pressed.includes('198/2') ? [doc, { ...alert, active: false }] : [alert, { ...doc, active: false }])
    if (tool === 'desk_snapshot') return JSON.stringify(args.window === doc.title ? { window: doc, nodes, truncated: false } : { window: alert, nodes: shown, truncated: false })
    if (tool === 'desk_press') { pressed.push(String(args.id)); up ||= args.id === '198/1'; return '{"ok":true,"how":"ax"}' }
    return '{"ok":true}'
  })
  const r = await plan({ steps: [{ app: 'R89Kit' }, { click: 'Alert now' }, { click: 'Tool' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.saved, undefined, JSON.stringify(r))
  assert.equal(r.escalate?.step, 2, JSON.stringify(r))
  assert.equal(r.escalate?.reason, TEXT)
  assert.deepEqual(pressed, ['198/1'])
})

test('a macOS key into an untitled alert the plan\'s own step opened goes into the alert, as before the rule for an alert open before the plan', { skip: process.platform !== 'darwin' }, async (t) => {
  const doc = { id: '198', title: 'r89aN3', role: 'window', pid: 1966, active: true, app: 'R89Kit' }
  const alert = { id: '201', title: '', role: 'dialog', pid: 1966, active: true, app: 'R89Kit' }
  const nodes = [{ ref: 0, id: '198/0', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }, { ref: 1, id: '198/1', parent: 0, depth: 1, role: 'button', name: 'Alert now', states: ['showing'] }]
  let up = false; const keys: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(up ? [alert, { ...doc, active: false }] : [doc])
    if (tool === 'desk_snapshot') return JSON.stringify(args.window === doc.title ? { window: doc, nodes, truncated: false } : { window: alert, nodes: [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }], truncated: false })
    if (tool === 'desk_press') { up ||= args.id === '198/1'; return '{"ok":true,"how":"action"}' }
    if (tool === 'desk_key') { keys.push(args.keys); up = false }
    return '{"ok":true}'
  })
  const r = await plan({ steps: [{ app: 'R89Kit' }, { click: 'Alert now' }, { key: 'return' }] })
  assert.deepEqual(keys, ['return'], JSON.stringify(r))
})

test('a macOS alert of the app open before the plan stops every input step, named or not, with nothing sent, an unnamed close closes it, and an unnamed close among two such dialogs stops', { skip: process.platform !== 'darwin' }, async (t) => {
  for (const [step, want, variant] of [[{ click: 'Tool', window: 'r89aN3' }, TEXT, ''], [{ click: 'Tool' }, TEXT, ''], [{ click: 'OK' }, TEXT, ''], [{ click: 'Delete' }, TEXT, 'menu'], [{ click: 'Delete', fp: { role: 'button', name: 'Tool', parent: 'r89aN3', nth: 0 } }, TEXT, ''], [{ click: 'Delete' }, TEXT, 'hidden'], [{ click: 'Delete' }, TEXT, 'unbounded'], [{ click: 'Delete' }, TEXT, 'static'], [{ check: 'Wrap', on: true }, TEXT, ''], [{ set: 'Name', value: 'x' }, TEXT, ''], [{ menu: 'Edit > Delete' }, TEXT, 'menu'], [{ type: 'x' }, TEXT, ''], [{ key: 'return' }, TEXT, ''], [{ close: true, window: 'r89aN3' }, TEXT, ''], [{ close: true }, '', ''], [{ close: true }, SEVERAL, 'two dialogs'], [{ type: 'x' }, ALONE, 'front'], [{ key: 'return' }, ALONE, 'front'], [{ click: 'OK' }, ALONE, 'front'], [{ close: true }, '', 'front'], [{ close: true }, SEVERAL, 'two front'], [{ close: true }, SEVERAL, 'two second front'], [{ click: 'Tool' }, TEXT, 'wait'], [{ key: 'return' }, TEXT, 'wait'], [{ type: 'x' }, TEXT, 'wait']] as const) {
    rmSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, { force: true })
    const doc = { id: '198', title: 'r89aN3', role: 'window', pid: 1966, active: !['front', 'two front', 'two second front', 'wait'].includes(variant), app: 'R89Kit' }
    const alert = { id: '201', title: '', role: 'dialog', pid: 1966, active: variant === 'front' || variant === 'two front' || variant === 'wait', app: 'R89Kit' }
    const second = { id: '202', title: '', role: 'dialog', pid: 1966, active: variant === 'two second front', app: 'R89Kit' }
    const at = variant === 'unbounded' ? { bounds: [10, 10, 40, 20] } : {}
    const nodes = [{ ref: 0, id: '198/0', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }, { ref: 1, id: '198/2', parent: 0, depth: 1, role: 'button', name: 'Tool', states: ['showing'], ...at }, { ref: 2, id: '198/3', parent: 0, depth: 1, role: variant === 'static' ? 'text' : 'button', name: variant === 'menu' ? 'Wrap' : 'Delete', states: variant === 'hidden' ? [] : ['showing'] }, { ref: 3, id: '198/4', parent: 0, depth: 1, role: 'checkbox', name: 'Wrap', states: ['showing'] }, { ref: 4, id: '198/5', parent: 0, depth: 1, role: 'textbox', name: 'Name', states: ['showing'] }, { ref: 5, id: '198/6', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] }, { ref: 6, id: '198/7', parent: 5, depth: 2, role: 'menu', name: 'Edit', states: ['showing'] }, { ref: 7, id: '198/8', parent: 6, depth: 3, role: 'menuitem', name: 'Delete', states: [] }]
    const own = [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }, { ref: 1, id: '201/1', parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'] }, { ref: 2, id: '201/2', parent: 0, depth: 1, role: 'button', name: 'Cancel', states: ['showing'] }, { ref: 3, id: '201/3', parent: 0, depth: 1, role: 'button', name: 'Delete', states: ['showing'] }]
    let open = true; const input: string[] = []; let focused = 0
    await daemon(t, ({ tool, args }) => {
      if (tool === 'desk_windows') return JSON.stringify([doc, ...(open ? [alert] : []), ...(variant.startsWith('two') ? [second] : [])].sort((a, b) => +b.active - +a.active))
      if (tool === 'desk_focus') focused += 1
      if (tool === 'desk_snapshot') return JSON.stringify(args.window === doc.title ? { window: doc, nodes, truncated: false } : { window: alert, nodes: own, truncated: false })
      if (['desk_press', 'desk_type', 'desk_key', 'desk_click', 'desk_close'].includes(tool)) input.push(tool)
      if (tool === 'desk_key' && args.keys === 'Escape') open = false
      return '{"ok":true}'
    })
    const r = await plan({ steps: [variant === 'wait' ? { wait: 'r89aN3' } : { app: 'R89Kit' }, step] })
    if (!want) { assert.equal(r.ok, true, JSON.stringify(r)); assert.deepEqual(input, ['desk_key']); continue }
    assert.equal(r.escalate?.step, 1, `${JSON.stringify(step)} ${JSON.stringify(r)}`)
    assert.equal(r.escalate?.reason, want, JSON.stringify(step))
    assert.deepEqual(input, [], JSON.stringify(step))
    if (variant === 'wait') assert.equal(focused, 0)
  }
})

test('a plan with no app step acts in an untitled dialog of another process right after a plan of another app left its chain at an untitled window, and a call of the same process continues the chain by its pid', { skip: process.platform !== 'darwin' }, async (t) => {
  const other = { id: '301', title: '', role: 'dialog', pid: 3000, active: true, app: 'Other' }
  const alert = { id: '201', title: '', role: 'dialog', pid: 1966, active: true, app: 'R89Kit' }
  let ws = [other]; const pressed: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(ws)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: ws[0], nodes: [{ ref: 0, id: `${ws[0]?.id}/0`, parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }, { ref: 1, id: `${ws[0]?.id}/1`, parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'] }], truncated: false })
    if (tool === 'desk_press') pressed.push(args.id)
    return '{"ok":true}'
  })
  rmSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, { force: true })
  assert.equal((await plan({ steps: [{ app: 'Other' }] })).ok, true)
  ws = [alert]
  const fresh = await plan({ steps: [{ click: 'OK' }] })
  assert.equal(fresh.ok, true, JSON.stringify(fresh))
  assert.deepEqual(pressed, ['201/1'])
  ws = [other]
  const same = await plan({ steps: [{ click: 'OK' }] })
  assert.equal(same.escalate?.reason, ALONE.replace('R89Kit', 'Other'), JSON.stringify(same))
  assert.deepEqual(pressed, ['201/1'])
})

test('a call that continues the chain of a plan that named its app stops at an untitled dialog of the app open before the plan, also when it is retried from a later step', { skip: process.platform !== 'darwin' }, async (t) => {
  const alert = { id: '201', title: '', role: 'dialog', pid: 1966, active: true, app: 'R89Kit' }
  const doc = { id: '198', title: 'r89aN3', role: 'window', pid: 1966, active: false, app: 'R89Kit' }
  const keys: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([alert, doc])
    if (tool === 'desk_snapshot') return JSON.stringify(args.window === doc.title ? { window: doc, nodes: [{ ref: 0, id: '198/0', parent: -1, depth: 0, role: 'window', name: doc.title, states: ['showing'] }], truncated: false } : { window: alert, nodes: [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }, { ref: 1, id: '201/1', parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'] }], truncated: false })
    if (tool === 'desk_key') keys.push(args.keys)
    return '{"ok":true}'
  })
  assert.equal((await plan({ steps: [{ app: 'R89Kit' }] })).ok, true)
  const steps = [{ verify: 'OK' }, { key: 'return' }]
  for (const r of [await plan({ steps }), await plan({ steps, start: 1 })]) {
    assert.equal(r.escalate?.step, 1, JSON.stringify(r))
    assert.equal(r.escalate?.reason, ALONE, JSON.stringify(r))
  }
  assert.deepEqual(keys, [])
  const pending = `${process.env.SHUTTLE_FLOWS}/.pending.json`
  const held = JSON.parse(readFileSync(pending, 'utf8')) as Record<string, Record<string, unknown>>
  for (const [field, value] of [['at', Date.now() - 11 * 60 * 1000], ['window', 'elsewhere']] as const) {
    writeFileSync(pending, JSON.stringify({ '': { ...held[''], [field]: value, ...(field === 'window' ? { pid: 77 } : {}) } }))
    const r = await plan({ steps: [{ key: 'shift+a' }] })
    assert.equal(r.ok, true, `${field} ${JSON.stringify(r)}`)
  }
  rmSync(pending, { force: true })
  const fresh = await plan({ steps: [{ key: 'shift+a' }] })
  assert.equal(fresh.ok, true, JSON.stringify(fresh))
})

test('a macOS call that continues the chain of an app step that failed at its first step stops at an untitled dialog of the app, and a type or key that names its window stops with no step before it', { skip: process.platform !== 'darwin' }, async (t) => {
  const alert = { id: '201', title: '', role: 'dialog', pid: 1966, active: true, app: 'R89Kit' }
  const doc = { id: '198', title: 'r89aN3', role: 'window', pid: 1966, active: false, app: 'R89Kit' }
  let stale = true; const keys: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ ...alert, ...(stale ? { stale: true } : {}) }, doc])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: alert, nodes: [{ ref: 0, id: '201/0', parent: -1, depth: 0, role: 'dialog', name: 'alert', states: ['showing'] }], truncated: false })
    if (tool === 'desk_key' || tool === 'desk_type') keys.push(args.keys ?? args.text)
    return '{"ok":true}'
  })
  rmSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, { force: true })
  const failed = await plan({ steps: [{ app: 'R89Kit' }] })
  assert.equal(failed.escalate?.step, 0, JSON.stringify(failed))
  stale = false
  const next = await plan({ steps: [{ key: 'return' }] })
  assert.equal(next.escalate?.reason, ALONE, JSON.stringify(next))
  rmSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, { force: true })
  for (const step of [{ type: 'x', window: 'r89aN3' }, { key: 'return', window: 'r89aN3' }]) {
    const r = await plan({ steps: [step] })
    assert.equal(r.escalate?.reason, ALONE, JSON.stringify(r))
  }
  assert.deepEqual(keys, [])
})

