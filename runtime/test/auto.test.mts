import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import type { Call } from './daemon.mts'

type Lib = typeof import('../lib/gui_plan.mts')
const load = async (input: string): Promise<Lib['main']> => {
  process.env.SHUTTLE_INPUT = input
  return ((await import(`../lib/gui_plan.mts?${input}`)) as Lib).main
}
const background = await load('background')
const auto = await load('auto')

type W = { id: string; title: string; role: string; pid: number; active: boolean }
type Plan = typeof auto

const user = (): W => ({ id: 'c', title: 'Calculator', role: 'window', pid: 9, active: true })
const notepad = (): W => ({ id: '1', title: 'Untitled - Notepad', role: 'window', pid: 4, active: false })
const NOTE = 'unverifiable: Sent 2 char(s) via PostMessage (not verified).'

function desktop(typed: (before: string, text: string) => string, note: boolean) {
  const ws = [user(), notepad()]
  let doc = 'x'
  const calls: Call[] = []
  const reply = (c: Call) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify(ws)
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes: [{ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [], value: doc }], truncated: false })
    if (c.tool === 'desk_focus') { ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: true }; return JSON.stringify({ ok: true, title: ws[1].title, pid: 4 }) }
    if (c.tool === 'desk_type') {
      if (c.args.window === undefined) { doc += String(c.args.text); return JSON.stringify({ ok: true, chars: 2, input: 'foreground' }) }
      doc = typed(doc, String(c.args.text))
      return JSON.stringify({ ok: true, chars: 2, ...(note ? { unverified: NOTE } : {}) })
    }
    return '{"ok":true}'
  }
  return { calls, reply, doc: () => doc }
}

const tools = (calls: Call[]) => calls.map((c) => `${c.tool} ${JSON.stringify(c.args)}`)

async function run(t: { after(fn: () => void): void }, plan: Plan, typed: (before: string, text: string) => string, note: boolean) {
  const d = desktop(typed, note)
  await daemon(t, d.reply)
  const r = await plan({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  return { r, calls: d.calls, doc: d.doc() }
}

test('an auto plan sends the same desk calls as a background plan when nothing falls back', async (t) => {
  for (const [typed, note] of [[(b: string, s: string) => b + s, false], [(b: string, s: string) => b + s, true]] as const) {
    const bg = await run(t, background, typed, note)
    const au = await run(t, auto, typed, note)
    assert.equal(bg.r.ok, true, JSON.stringify(bg.r))
    assert.equal(au.r.ok, true, JSON.stringify(au.r))
    assert.deepEqual(tools(au.calls), tools(bg.calls))
  }
})

test('typing that did not land and left the front alone runs once in the foreground under auto, and escalates under background', async (t) => {
  const bg = await run(t, background, (b) => b, true)
  assert.equal(bg.r.ok, false)
  assert.match(String(bg.r.escalate?.reason), /did not show/)
  const au = await run(t, auto, (b) => b, true)
  assert.equal(au.r.ok, true, JSON.stringify(au.r))
  assert.equal(au.doc, 'xhi')
  assert.deepEqual(au.calls.filter((c) => c.tool === 'desk_focus').map((c) => c.args), [{ window: 'Untitled - Notepad', pid: 4 }])
  assert.equal(au.r.trace?.[1]?.how, 'foreground')
})

test('typing that landed in part is never typed again in the foreground', async (t) => {
  const au = await run(t, auto, (b) => `${b}h`, true)
  assert.equal(au.r.ok, false)
  assert.match(String(au.r.escalate?.reason), /did not show/)
  assert.equal(au.doc, 'xh')
  assert.ok(!au.calls.some((c) => c.tool === 'desk_focus'))
})

test('after auto ran a step in the foreground, a dialog of the app in front fails no later step', async (t) => {
  const ws: (W & { owner?: string })[] = [user(), notepad()]
  const calls: Call[] = []
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify(ws)
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes: [], truncated: false })
    if (c.tool === 'desk_key') {
      ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: false }
      ws.push({ id: 's', title: 'Save as', role: 'dialog', pid: 4, active: true, owner: '1' })
      return JSON.stringify({ ok: true, input: 'foreground' })
    }
    return JSON.stringify({ ok: true, chars: 4 })
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { key: 'ctrl+shift+s' }, { type: 'name' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.trace?.[1]?.input, 'foreground')
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_type').map((c) => c.args), [{ text: 'name' }])
})

test('an auto macOS menu step fronts the plan window and runs the entry in the foreground, where background refuses', { skip: process.platform !== 'darwin' }, async (t) => {
  const nodes = [
    { ref: 0, id: 'r', parent: -1, depth: 0, role: 'window', name: 'Untitled - Notepad', states: [] },
    { ref: 1, id: 'mb', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 2, id: 'm', parent: 1, depth: 2, role: 'menu', name: 'Format', states: ['showing'], bounds: [10, 0, 40, 20] },
    { ref: 3, id: 'f', parent: 2, depth: 3, role: 'menuitem', name: 'Font', states: ['showing'], bounds: [10, 30, 80, 20] },
  ]
  const plan = async (main: Plan) => {
    const ws = [user(), notepad()]
    const calls: Call[] = []
    await daemon(t, (c) => {
      calls.push(c)
      if (c.tool === 'desk_windows') return JSON.stringify(ws)
      if (c.tool === 'desk_snapshot') return JSON.stringify({ window: ws[1], nodes, truncated: false })
      if (c.tool === 'desk_focus') { ws[0] = { ...user(), active: false }; ws[1] = { ...notepad(), active: true }; return JSON.stringify({ ok: true, title: ws[1].title, pid: 4 }) }
      if (c.tool === 'desk_press') return JSON.stringify({ ok: true, how: 'action' })
      return '{"ok":true}'
    })
    return { r: await main({ steps: [{ app: 'Notepad' }, { menu: 'Format > Font' }] }), calls }
  }
  const bg = await plan(background)
  assert.equal(bg.r.ok, false)
  assert.match(String(bg.r.escalate?.reason), /menu bar belongs to the front app/)
  const au = await plan(auto)
  assert.equal(au.r.ok, true, JSON.stringify(au.r))
  assert.deepEqual(au.calls.filter((c) => c.tool === 'desk_focus').map((c) => c.args), [{ window: 'Untitled - Notepad', pid: 4 }])
  assert.deepEqual(au.calls.filter((c) => c.tool === 'desk_press').map((c) => c.args), [{ id: 'f' }])
  assert.equal(au.r.trace?.[1]?.input, 'foreground')
})

test('a field auto cannot read is never typed into again in the foreground', async (t) => {
  const calls: Call[] = []
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([user(), notepad()])
    if (c.tool === 'desk_snapshot') return JSON.stringify({ window: notepad(), nodes: [{ ref: 0, id: 'd', parent: -1, depth: 0, role: 'document', name: '', states: [] }], truncated: false })
    if (c.tool === 'desk_read') return { text: 'the element answers no text', error: true }
    if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: 2, unverified: NOTE })
    return '{"ok":true}'
  })
  const r = await auto({ steps: [{ app: 'Notepad' }, { type: 'hi' }] })
  assert.equal(r.ok, false)
  assert.match(String(r.escalate?.reason), /did not show/)
  assert.ok(!calls.some((c) => c.tool === 'desk_focus'))
  assert.equal(calls.filter((c) => c.tool === 'desk_type').length, 1)
})
