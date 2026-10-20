import { execFileSync } from 'node:child_process'
import { appendFileSync, chmodSync, existsSync, mkdirSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs'
import { win32 } from 'node:path'
import { setTimeout as sleep } from 'node:timers/promises'
import { isDeepStrictEqual } from 'node:util'
import * as desk from './desk.mts'
import type { Win } from './desk.mts'
import { cut, ring, unpng } from './png.mts'

type Step = {
  app?: string; title?: string; menu?: string; click?: string; check?: string; on?: boolean; set?: string; value?: string
  type?: string; key?: string; wait?: string; expect?: string; verify?: string; close?: boolean; role?: string; window?: string
  timeout_ms?: number; enabled?: boolean; fp?: Fp; seen?: string; blank?: Fp
}
type Reads = { keys?: boolean; menus?: boolean }
type Kept = Reads & { id: string; snap: desk.Snap; nodes: Node[] }
type Fp = { role: string; name: string; parent: string; nth: number; label?: string }
type Node = { ref: number; id: string; depth: number; role: string; name: string; value: string; disabled: boolean; parent: number; shortcut: string; checked: boolean; shown: boolean; bounds?: [number, number, number, number] }
type Hit = { n: Node; i: number; how: string; p?: number; margin?: number }
type Flow = { name: string; params: Record<string, string>; steps: Step[]; idempotent: boolean; runs: number; saved_at: string; rewrites: number; usage?: desk.Usage }
type Chain = { at: number; window: string | null; pid: number | null; name?: string; named?: boolean; params: Record<string, string>; steps: Step[]; dirty: boolean; saved?: boolean; escalated?: boolean; usage?: desk.Usage }
type Params = { steps?: Step[]; flow?: string; params?: Record<string, string>; save?: string; start?: number; find?: string; reindex?: boolean; locate?: { target: string; press?: boolean } }
type Rec = {
  i: number; op?: string; ms?: number; served?: unknown[]; how?: string; via?: string; window?: string; seen?: string; submit?: boolean
  blank?: Fp; fp?: Fp; was?: boolean | 'mixed' | null; now?: boolean; noop?: boolean; state?: string; opened?: number; path?: string[]; pick?: number[]
  focused?: string; p?: number; p_zoom?: number | null; p_full?: number | null; verdict?: Vision; ground_hit?: string; checked?: string; value?: string; ref?: string; unsaved?: boolean
  unverified?: string; confirmed?: boolean; fg?: boolean
}
type Vision = { on?: boolean; ok?: boolean; why?: string }
type Seen = { window?: string | null; windows?: string[]; outline?: string }
type Escalation = Seen & { reason: string; step?: number; plan_step?: Step }
type Trace = { i: number; op?: string; how?: string; input?: 'foreground'; ms?: number; was?: 'on' | 'off' | 'mixed'; now?: 'on' | 'off'; noop?: boolean }
export type Result = { ok?: boolean; done?: number; ms?: number; trace?: Trace[]; escalate?: Escalation; served?: desk.Served[]; [key: string]: unknown }

const OS = process.platform
// with [desk] input = "background" a plan drives its target window, never the front one
const BEHIND = process.env.SHUTTLE_INPUT === 'background' || process.env.SHUTTLE_INPUT === 'auto'
const AUTO = process.env.SHUTTLE_INPUT === 'auto'
const NO_PIXELS = 'background gui_plan does not read the screen, which shows whatever covers the plan\'s window'
const FLOWS = process.env.SHUTTLE_FLOWS ?? ''
const LOG = `${FLOWS}/.runs.jsonl`
const LOG_MAX = 1024 * 1024
const INDEX = `${FLOWS}/INDEX`
const PENDING = `${FLOWS}/.pending.json`
const CHAIN_AGE = 10 * 60 * 1000
const PRIVATE = { mode: 0o600 }
const ACTIVE = new Set(['button', 'checkbox', 'menuitem', 'menu', 'tab', 'textbox', 'spinbutton', 'slider', 'combobox', 'radio', 'link', 'switch', 'option', 'treeitem', 'togglebutton', 'searchbox', 'menuitemcheckbox', 'menuitemradio'])
const TOGGLES = 'checkbox|switch|togglebutton|menuitemcheckbox'
const TOGGLE_ROLES = new Set(TOGGLES.split('|'))
const ITEMS = new Set(['cell', 'listitem'])
const LISTS = new Set(['treegrid', 'list', 'tree', 'table'])
const FIELDS = 'textbox|searchbox|spinbutton|slider|combobox'
const FIELD_ROLES = new Set(FIELDS.split('|'))
const SHOWN_ROLES = new Set([...ACTIVE, 'dialog', 'alert'])
const MARGIN = 0.25
const CROP = { w: 640, h: 400 }
const SMALL = 5_000
const ZOOM_BAR = 0.65
const FULL_BAR = 0.95
const RING = 14
const PICK_MARGIN = 0.6
const SETTLE = 150
const OPENING = 1000
const INERT_KEY = /^shift\+[^+]$/i
const ARROW_KEY = /^(shift\+)?(arrow)?(up|down|left|right)$/i
const SAID: Record<string, string[]> = { link: ['link'], button: ['button', 'togglebutton'] }
const FILLER = new Set(['the', 'a', 'an', 'of', 'in', 'on', 'at', 'to', 'for', 'with', 'and', 'or', 'by', 'from', 'into', 'this', 'that', 'its', 'it', 'is', 'button', 'link', 'tab', 'field', 'box', 'icon', 'toggle', 'dropdown', 'input', 'control', 'item', 'entry'])
const OPS = ['app', 'menu', 'click', 'check', 'set', 'type', 'key', 'wait', 'expect', 'verify', 'close']
const TEXT_CUT = '\n(the outline shows only the start of the window: desk snapshot or find reads the rest)'
const TREE_CUT = '\n(this accessibility read stopped before the end of the window: the elements past the cut are missing here; narrowing the view, e.g. by scrolling or opening a pane, brings them in)'
const SHELL_TARGET = /\b(dock|panel|taskbar|desktop)\b/i
const ANDROID_PACKAGE = /^[a-z][a-z0-9_]*(\.[a-z][a-z0-9_]*)+$/
const CHOOSER_BY_OS: Partial<Record<NodeJS.Platform, RegExp>> = { linux: /File Chooser Widget/, win32: /^\s*- (combobox|textbox) "File name:?"/m, darwin: /^\s*- text "Save As:"/m }
const CHOOSER = CHOOSER_BY_OS[OS] ?? /(?!)/
const SELECT_ALL = OS === 'darwin' ? 'cmd+a' : 'ctrl+a'
const known = new Set<string>()
let busNames: string[] | null = null
let gitWorks: boolean | undefined
let lastSeen = ''
let recording = false
let typed = false
let pressed: { title: string; at: number } | null = null
let target: Win | null = null
let listed: Win[] = []
let targetSince = new Set<string>()
let adopted = new Set<string>()
let doubt: string | null = null
let unheard: string | undefined
let userFront: Win | null = null
let BG = BEHIND
let fronted = false
const foreground = { steps: 0 }
let T: Record<string, number> = {}

/** Runs one gui_plan call: a plan, a saved flow, or a flow-library verb. */
export async function main(p: Params): Promise<Result> {
  return { ...(await perform(p)), usage: desk.usage }
}

async function perform(p: Params): Promise<Result> {
  const t0 = Date.now()
  if (!FLOWS) return { ok: false, error: 'SHUTTLE_FLOWS names no flows directory' }
  try { flowsDir() } catch {}
  if (p.reindex) { repo(); writeIndex(); git('add', '-A'); git('commit', '-q', '-m', 'sync from the flow library'); return { ok: true } }
  if (p.find) return await find(p.find)
  if (p.locate) return await locate(p.locate.target, p.locate.press ?? false)
  if (p.save && !p.steps?.length && !p.flow) return await promote(p.save, p.params)
  let flow: Flow | null = null
  try { if (p.flow) flow = loadFlow(p.flow) } catch (e: unknown) { return { ok: false, done: 0, ms: 0, escalate: { reason: e instanceof Error ? e.message : String(e) } } }
  const params = { ...(flow?.params ?? {}), ...(p.params ?? {}) }
  const plan: Step[] = (flow ? flow.steps : p.steps ?? []).filter((s) => !Object.keys(s).every((k) => ['screenshot', 'timeout_s', 'max_edge'].includes(k)))
  if (!plan.length) return { ok: false, error: 'pass steps, flow or find' }
  recording = !flow && !!plan[0]?.app && !p.start
  if (p.save && !flow && !p.start && !idempotent(plan)) {
    const c = pending()
    const was = c?.steps.filter((s) => !s.close) ?? []; const now = plan.filter((s) => !s.close)
    if (c && was.length === now.length && was.every((s, i) => bare(s, c.params) === bare(now[i] ?? {}, params))) {
      const r = await promote(p.save, p.params, true)
      if (r.ok) return { ...r, ran: false, why: 'these steps passed in the last plan, so they were saved without running them again; call without save to run them' }
    }
  }
  const trace: Trace[] = []; const log: Record<string, unknown>[] = []
  const bad = plan.findIndex((s) => OPS.filter((k) => k in s).length !== 1)
  if (bad >= 0) return { ok: false, done: 0, ms: 0, trace, escalate: { step: bad, plan_step: plan[bad], reason: `step ${bad} needs exactly one op of ${OPS.join(', ')}; nothing ran` } }
  const unfilled = (s: Step) => Object.values(s).flatMap((v) => typeof v === 'string' ? [...v.matchAll(/\{\{(\w+)(?::base)?\}\}/g)].map((x) => x[1] ?? '') : []).find((n) => params[n] === undefined)
  const gap = plan.findIndex((s, i) => i >= (p.start ?? 0) && unfilled(s))
  const gapStep = plan[gap]
  if (gapStep) return { ok: false, done: 0, ms: 0, trace, escalate: { step: gap, plan_step: gapStep, reason: `no value for {{${unfilled(gapStep)}}}: pass it in params; nothing ran` } }
  BG = BEHIND
  const ws0 = await desk.windows(); for (const w of ws0) known.add(w.title)
  if (BG) { target = null; adopted = new Set(); doubt = null; fronted = false; userFront = desk.front(ws0); const c = pending(); const was = c && Date.now() - c.at < CHAIN_AGE ? ws0.find((w) => w.title === c.window && w.pid === c.pid) : undefined; if (was) follow(was, ws0) }
  const vetted = new Set<string>()
  const startTitles = new Set(known)
  const start = frontOf(ws0); const startWindow = start?.title ?? null; const startPid = start ? start.pid : null
  const startAlert = !start?.title.trim() && isAlert(await outline(start))
  const recs: Rec[] = []
  let escalate: Escalation | null = null; let done = plan.length
  for (const [i, planned] of plan.entries()) {
    if (i < (p.start ?? 0)) continue
    const s = fill(planned, params); T = {}; const rec: Rec = { i, op: OPS.find((k) => k in s) }
    const ts = Date.now(); const calls = desk.served.length
    try {
      const ws = i === (p.start ?? 0) ? ws0 : await desk.windows(); const f = BG ? frontOf(ws) : desk.front(ws)
      if (BG && doubt && !s.app && !s.wait) throw new Surprise(doubt, await seen())
      if (BG && i > (p.start ?? 0)) stolen(ws, i - 1)
      const m = s.close ? null : await hiddenModal(ws, f)
      if (m) throw new Surprise(modalBlocks(m), await seen())
      const r = reads(s)
      const pre = f && (!known.has(f.title) || !f.title.trim()) && !vetted.has(`${f.id} ${f.pid} ${f.title}`) && !(i === (p.start ?? 0) && startAlert) && !(s.window && norm(f.title).includes(norm(s.window))) ? await look(f, r) : null
      const o = pre?.text ?? ''
      if (f && blocked(o)) throw new Surprise(unanswered(f.title), await seen())
      if (f && isAlert(o)) throw new Surprise(`alert "${f.title}" is in front`, await seen())
      if (f && pre) vetted.add(`${f.id} ${f.pid} ${f.title}`)
      const kept = f && pre?.snap && pre.nodes && f.title !== pressed?.title ? { ...r, id: f.id, snap: pre.snap, nodes: pre.nodes } : undefined
      const prev = plan[i - 1]
      unheard = undefined
      const runs = desk.foreground.runs + foreground.steps
      await run(s, rec, kept).catch(async (e: unknown) => {
        const late = (x: unknown) => x instanceof Surprise && x.late
        if (!late(e) || !(prev?.menu || prev?.click || prev?.key)) throw e
        for (const end = Date.now() + 3000; ; await sleep(200)) {
          try { return await run(s, rec) } catch (again: unknown) { if (!late(again) || Date.now() > end) throw again }
        }
      })
      if (unheard && !rec.unverified) rec.unverified = unheard
      if (desk.foreground.runs + foreground.steps > runs) { rec.fg = true; fronted = true; BG = false }
      const before = recs[i - 1]
      if (before && (s.wait || s.verify || s.expect)) before.confirmed = true
    } catch (e: unknown) {
      const sp = e instanceof Surprise ? e : new Surprise(e instanceof Error ? e.message : String(e), await seen())
      const step = e instanceof Stolen ? e.step : i
      const note = step === i ? recs[i - 1]?.unverified : undefined
      const theft = BG && !(e instanceof Stolen) ? taken(await desk.windows()) : null
      const reason = `${sp.reason}${note ? ` (the step before was sent but not confirmed: ${note})` : ''}${theft ? `; meanwhile ${theft}` : ''}`
      escalate = { step, plan_step: plan[step] ?? planned, reason, ...sp.seen }; done = step
    }
    rec.ms = Date.now() - ts; if (desk.served.length > calls) rec.served = desk.served.slice(calls)
    log.push({ ...rec, ...T }); recs[i] = rec
    trace.push({ i, op: rec.op, how: rec.how, ...(rec.fg || rec.how === 'foreground' ? { input: 'foreground' as const } : {}), ms: rec.ms, ...(rec.was !== undefined ? { was: rec.was === 'mixed' ? 'mixed' : rec.was ? 'on' : 'off', now: rec.noop || s.verify || escalate ? undefined : s.on === false ? 'off' : 'on' } : {}), ...(rec.now !== undefined ? { now: rec.now ? 'on' : 'off' } : {}), ...(rec.noop ? { noop: true } : {}) })
    if (escalate) break
  }
  const last = plan.at(-1) ?? {}
  let opened = false
  let end: Win | null = null
  if (!escalate) {
    const ts = Date.now(); let ws = await desk.windows(); const front = frontOf(ws); const was = front?.title
    const inert = (last.type !== undefined && !last.type.includes('\n')) || (last.key !== undefined && (INERT_KEY.test(last.key) || (ARROW_KEY.test(last.key) && front?.role === 'window')))
    if ((last.key || last.type !== undefined || last.click) && !recs.at(-1)?.submit && !inert) {
      await until((ws) => frontOf(ws)?.title !== was, 480, ws)
      await sleep(80); ws = await desk.windows()
    }
    const acted = !!(last.click || last.check || last.set || last.menu || last.key || last.close || last.type !== undefined)
    const f = frontOf(ws); end = f; const o = f && (acted || f.role !== 'window') ? await outline(f) : ''
    log.push({ i: plan.length, op: 'end', ms: Date.now() - ts, window: f?.title ?? null })
    const left = f && !startTitles.has(f.title) && isDialog(o) && !(last.wait || last.app || (last.window && norm(f.title).includes(norm(last.window))))
    const theft = BG ? taken(ws) : null
    const open = recs.find((r) => r?.unverified && !r.confirmed)
    if (theft) { escalate = { step: plan.length - 1, reason: theft, ...(await seen()) }; done = plan.length - 1 }
    else if (open) { escalate = { step: open.i, reason: `step ${open.i} (${open.op}) was sent but not confirmed, and no later step read its effect, so the plan cannot say it landed: ${open.unverified}`, ...(await seen()) }; done = open.i }
    else if (f && blocked(o) && recs.at(-1)?.how !== 'wm') { escalate = { step: plan.length, reason: unanswered(f.title), ...(await seen()) }; done = plan.length }
    else if (f && (isAlert(o) || left)) { escalate = { step: plan.length, reason: `the plan ended with "${f.title || o.split('\n')[0]}" still in front`, ...(await seen()) }; done = plan.length }
    opened = !!f && !startTitles.has(f.title) && isDialog(o)
  }
  let saved: string | undefined
  const jevs = recs.filter((r) => r?.pick).length
  const settled = !escalate && !opened
  if (flow) {
    putPending(null)
    if (settled && recs.every((r) => !r?.unsaved)) {
      const rewrites = recs.filter((r, i) => r?.fp && !isDeepStrictEqual(r.fp, flow.steps[i]?.fp))
      flow.runs += 1
      if (rewrites.length) {
        recs.forEach((r, i) => { const st = flow.steps[i]; if (!st) return; if (r?.fp) st.fp = r.fp; if (r?.seen) st.seen = r.seen })
        flow.rewrites += 1; flow.saved_at = new Date().toISOString(); saveFlow(flow, `replay re-resolved ${rewrites.length} step(s)`)
      }
      saved = flow.name
    }
  } else {
    // a task done over several calls is one chain: it starts at a call whose first step is an app, and later calls extend it from the window it left
    const ran = plan.slice(p.start ?? 0, done).map((s, k) => { const r = recs[(p.start ?? 0) + k]; const o: Step = { ...s }; if (r?.fp) o.fp = r.fp; if (r?.seen && !s.app) o.seen = r.seen; if (r?.blank) o.blank = r.blank; return o })
    const dirty = recs.some((r) => r?.unsaved)
    let chain: Chain | null = null; const prev = pending()
    if (plan[0]?.app && !(p.start ?? 0)) chain = { at: Date.now(), window: null, pid: null, params: { ...(p.params ?? {}) }, steps: ran, dirty, usage: plus({}, desk.usage) }
    else if (prev && Date.now() - prev.at < CHAIN_AGE && (prev.window === startWindow || (prev.pid !== null && prev.pid === startPid))) chain = { ...prev, at: Date.now(), params: { ...prev.params, ...(p.params ?? {}) }, steps: [...prev.steps, ...ran], dirty: prev.dirty || dirty, usage: plus(prev.usage, desk.usage) }
    if (chain) {
      if (p.save) { chain.name = slug(p.save); chain.named = true }
      const changes = chain.steps.some((s) => s.check || s.set || s.type !== undefined || (s.menu && s.on !== undefined))
      chain.escalated = !!escalate; chain.saved = false
      if ((settled || (!escalate && changes)) && !chain.dirty && chain.steps.some((s) => !s.app)) { saved = store(chain); chain.saved = !!saved }
      const front = escalate ? await focused() : end
      chain.window = front?.title ?? null
      if (front && !isShell(front)) chain.pid = front.pid
      putPending(chain)
    } else if (prev) putPending({ ...prev, saved: false, escalated: false })
  }
  const ms = Date.now() - t0
  try { if (statSync(LOG).size > LOG_MAX) rmSync(LOG) } catch {}
  try { appendFileSync(LOG, `${JSON.stringify({ at: new Date().toISOString(), flow: p.flow ?? null, ok: !escalate, ms, jevs, saved: saved ?? null, usage: desk.usage, steps: log })}\n`, PRIVATE) } catch {}
  const out: Result = { ok: !escalate, done, ms, trace }
  if (saved) out[flow ? 'flow' : 'saved'] = saved
  if (flow?.usage) out.recorded_usage = flow.usage
  if (escalate) out.escalate = escalate
  else out.window = end?.title ?? null
  const notes = recs.filter((r) => r?.unverified && !r.confirmed).map((r) => ({ step: r.i, note: r.unverified }))
  if (notes.length) out.unverified = notes
  return out
}

class Surprise extends Error {
  reason: string
  seen: Seen
  late: boolean
  constructor(reason: string, seen: Seen = {}, late = false) { super(reason); this.reason = reason; this.seen = seen; this.late = late }
}
// the app of the plan's window came to the front during a step that sent no fronting call
class Stolen extends Surprise {
  step: number
  constructor(reason: string, step: number) { super(reason); this.step = step }
}
function taken(ws: Win[]): string | null {
  const f = desk.front(ws)
  if (!f || !target || fronted || f.pid !== target.pid || (userFront && f.id === userFront.id)) return null
  return `"${f.title || f.app || 'the app'}" brought itself to the front though the plan sent no fronting call: background input cannot keep this app behind`
}
function stolen(ws: Win[], step: number) { const r = taken(ws); if (r) throw new Stolen(r, step) }

function terms(s: string) { return norm(s).split(/[^a-z0-9]+/).filter((w) => w.length > 1 && !FILLER.has(w)).map((w) => w.length > 3 ? w.replace(/s$/, '') : w) }
function norm(s: string) { return s.toLowerCase().replace(/[_…]|\.\.\.|:$/g, '').replace(/\s+/g, ' ').trim() }
function slug(s: string) { return norm(s).replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40) }
function tick(k: string, t0: number) { T[k] = (T[k] ?? 0) + Date.now() - t0 }
function* up<N extends { parent: number }>(nodes: N[], from: number): Generator<[number, N]> {
  for (let p = from; p >= 0;) { const n = nodes[p]; if (!n) return; yield [p, n]; p = n.parent }
}
function prob(j: desk.Judgment | undefined): number { return j?.probability ?? 0 }
function menus(nodes: Node[], from: number): Node[] {
  const out: Node[] = []
  for (const [, q] of up(nodes, from)) { if (q.role !== 'menu') break; out.push(q) }
  return out
}

function parse(snap: desk.Snap): Node[] {
  const kids = new Map<number, number>(); for (const n of snap.nodes) kids.set(n.parent, (kids.get(n.parent) ?? 0) + 1)
  const wraps = (n: desk.El) => { const out: desk.El[] = []; for (const [p, a] of up(snap.nodes, n.parent)) { if (kids.get(p) !== 1 || !/^(toolbar|group)$/.test(a.role)) break; out.push(a) } return out }
  const called = (n: desk.El) => {
    if (n.name.trim() || !ACTIVE.has(n.role) || FIELD_ROLES.has(n.role)) return n.name
    const d = n.description?.trim(); if (d) return d
    const w = wraps(n); return w.find((a) => a.name.trim())?.name || w.find((a) => a.description?.trim())?.description || ''
  }
  return snap.nodes.map((n) => ({
    ref: n.ref, id: n.id, depth: n.depth, role: n.role, name: called(n).slice(0, 80).trim(), value: n.value ?? '', disabled: n.states.includes('disabled'), parent: n.parent,
    shortcut: n.shortcut ?? '', checked: n.states.includes('checked') || n.states.includes('pressed'), shown: n.states.includes('showing'), bounds: n.bounds,
  }))
}

async function focused() { return frontOf(await desk.windows()) }
function focusedIn(ws: Win[]) {
  const f = desk.front(ws); if (f) return f
  const own = ws.filter((x) => x.title.trim() && ws.some((p) => p.active && !p.title.trim() && p.pid === x.pid))
  return own.find((x) => x.title === lastSeen) ?? own.at(-1) ?? null
}
// the window a step acts in: the front one, or in the background the plan's target or the newest window it owns
function frontOf(ws: Win[]) { return BG ? targetIn(ws) : focusedIn(ws) }
function targetIn(ws: Win[]): Win | null {
  listed = ws
  if (!target) return null
  const t = target
  const live = ws.find((x) => x.id === t.id) ?? null
  if (!live) doubt = `the plan's window "${t.title}" is gone, and background gui_plan never takes another window of that title for it`
  const mine = new Set(live ? [live.id] : [])
  const owned: Win[] = []
  for (let grew = true; grew;) {
    grew = false
    for (const x of ws) if (!mine.has(x.id) && belongs(x) && (!!x.title.trim() || x.role !== 'window')) { mine.add(x.id); owned.push(x); grew = true }
  }
  const leaves = owned.filter((x) => !owned.some((y) => y.owner === x.id))
  return leaves.find((x) => x.active) ?? leaves.at(-1) ?? live
  // the OS names a window's owner where it knows it; on macOS one new dialog of the app with no owner after input to the target is the target's
  function belongs(x: Win) {
    if (x.owner !== undefined) return mine.has(x.owner)
    if (OS !== 'darwin' || x.pid !== t.pid || x.role === 'window') return false
    if (adopted.has(x.id)) return true
    const fresh = ws.filter((y) => y.owner === undefined && y.pid === t.pid && y.role !== 'window' && !targetSince.has(y.id) && !adopted.has(y.id))
    if (fresh.length > 1) { doubt = `${fresh.length} dialogs of the app opened with no owner the OS names, and background gui_plan never guesses which one belongs to the plan's window`; return false }
    if (fresh[0]?.id !== x.id) return false
    adopted.add(x.id); return true
  }
}
function ours(x: Win, ws: Win[]) {
  for (let y: Win | undefined = x, k = 0; y && k < ws.length; y = ws.find((z) => z.id === y?.owner), k++) if (y.id === target?.id || adopted.has(y.id)) return true
  return false
}
function ownerOf(w: Win, ws: Win[]) {
  let x = w
  for (let k = 0; k < ws.length; k++) { const o = ws.find((y) => y.id === x.owner); if (!o) break; x = o }
  return x.id !== w.id ? x : ws.find((y) => y.id === target?.id && y.id !== w.id)
}
async function front(w: Win | null) {
  const to = w ? at(w) : {}
  if (to.window !== undefined) await desk.focus(to.window, to.pid)
  BG = false; fronted = true; foreground.steps += 1
}
function follow(w: Win, ws: Win[]) { target = w; doubt = null; targetSince = new Set(ws.map((x) => x.id)) }
// a macOS sheet has no window of its own, so its input goes to the window it hangs on
function at(w: Win | null): desk.Target {
  if (!BG || !w) return {}
  if (doubt) throw new Surprise(doubt)
  let x = w
  for (let o = OS === 'darwin' ? x.owner : undefined; o; o = x.owner) { const up = listed.find((y) => y.id === o); if (!up) break; x = up }
  const to = x
  unique(to)
  targetSince = new Set([...targetSince, ...listed.map((y) => y.id)])
  return { window: to.title, pid: to.pid }
}
function unique(w: Win) {
  if (BG && listed.some((y) => y.id !== w.id && y.pid === w.pid && y.title === w.title)) throw new Surprise(`another window of the app is also titled "${w.title}", and background input names its window by title, so it never guesses which one gets it`)
}
function heard(r: desk.Sent, rec: Rec) { if (BG && r.unverified && !rec.unverified) rec.unverified = r.unverified }
async function until(ok: (ws: Win[]) => boolean, ms: number, seed?: Win[]) {
  let ws = seed ?? await desk.windows()
  for (const end = Date.now() + ms; !ok(ws) && Date.now() < end;) ws = (await desk.wait(ws, end - Date.now())).windows
  return ws
}
async function current(bound = SETTLE) {
  const k = pressed; pressed = null; const t0 = Date.now()
  const ws = k ? await until((ws) => { const f = frontOf(ws); return !!f && f.title !== k.title }, bound - (t0 - k.at)) : await desk.windows()
  tick('settle', t0); return ws
}
function isShell(w: Win) { return !!w.shell || (OS === 'win32' && w.title === 'Program Manager') }
function titledIn(ws: Win[], title: string, pid: number | null) { return pid !== null && ws.some((x) => x.title === title && x.pid === pid) }
async function snapOf(w: Win, r: Reads = {}): Promise<desk.Snap> {
  return BG ? desk.snapshot(undefined, { id: w.id, ...r }) : desk.snapshot(w.title, { pid: w.pid, ...r })
}
function targetRole(s: Step) {
  if (s.verify !== undefined) return s.role ?? (s.value !== undefined ? FIELDS : s.on !== undefined ? `${TOGGLES}|menuitem` : undefined)
  return s.role ?? (s.check ? TOGGLES : s.set ? FIELDS : undefined)
}
function reads(s: Step): Reads {
  return { keys: !!(s.menu || s.click), menus: !!s.menu || (!!s.verify && s.on !== undefined) }
}
// the preamble's snapshot of the front window serves the step's first read of that window, which comes before any action
function reuse(kept: Kept | undefined, w: Win, r: Reads) {
  return kept?.id === w.id && !!kept.keys === !!r.keys && !!kept.menus === !!r.menus && kept.nodes.length > 1 ? kept : null
}
function render(nodes: Node[], max: number, interactive: boolean, limit = Infinity) {
  const q = (s: string) => s.slice(0, max).replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n')
  const lines: string[] = []; let size = -1
  for (const n of nodes) {
    if (size >= limit) break
    if (interactive && n.depth !== 0 && !SHOWN_ROLES.has(n.role)) continue
    const line = `${'  '.repeat(n.depth)}- ${n.role}${n.name ? ` "${q(n.name)}"` : ''}${n.disabled ? ' [disabled]' : ''}${n.checked ? ' [checked]' : ''}${n.shortcut ? ` [${n.shortcut}]` : ''}${n.id ? ` [id=${n.id}]` : ''}${n.value ? `: "${q(n.value)}"` : ''}`
    lines.push(line); size += line.length + 1
  }
  return lines.join('\n').slice(0, limit)
}
async function outline(w: Win | null, r: Reads = {}, kept?: Kept, max = 1800) {
  if (!w) return '(no focused window)'
  return (await look(w, r, kept, max)).text
}
async function look(w: Win, r: Reads = {}, kept?: Kept, max = 1800): Promise<{ text: string; snap?: desk.Snap; nodes?: Node[] }> {
  try { const k = reuse(kept, w, r); const s = k?.snap ?? await snapOf(w, r); const nodes = k?.nodes ?? parse(s); const full = render(nodes, 100, nodes.length > 60, max + 1); return { text: `${full.slice(0, max)}${s.truncated ? TREE_CUT : full.length > max ? TEXT_CUT : ''}`, snap: s, nodes } } catch (e: unknown) { return { text: String(e) } }
}
async function seen(): Promise<Seen> {
  const ws = await desk.windows(); const w = frontOf(ws)
  const windows = ws.filter((x) => x.title && !isShell(x)).map((x) => x.modal ? `${x.title} (modal dialog, ${x.modal})` : x.title)
  return { window: w?.title ?? null, windows, outline: await outline(w, { keys: true }) }
}
// a modal dialog of the focused app that the screen does not show takes the app's input: typing lands in it, and clicks do nothing
async function hiddenModal(ws: Win[], f: Win | null) {
  const of = (list: Win[]) => f && list.find((x) => x.modal && x.pid === f.pid)
  if (!of(ws)) return null
  await sleep(200)
  return of(await desk.windows()) ?? null
}
function modalBlocks(m: Win) { return `modal dialog "${m.title}" is open but ${m.modal}, and ${m.blocks ? `"${m.blocks}"` : 'its app'} takes no input until it closes: finish it, or close it with {close: true, window: "${m.title}"}` }
function blocked(o: string) { return /AT-SPI peer \S+ is not responding|no accessibility answer within/.test(o) }
function isChooser(o: string) { return isDialog(o) && CHOOSER.test(o) }
async function selectLine(w: Win | null) { if (OS === 'darwin') await desk.key(SELECT_ALL, at(w)); else { await desk.key('home', at(w)); await desk.key('shift+end', at(w)) } }
function unanswered(title: string) { return `"${title}" answers no accessibility calls (a dialog opened by an accessibility action blocks its app, and a key would deadlock it): close it with {close: true}, then reopen it by its keyboard shortcut` }
function isAlert(o: string) { return /^- alert/.test(o) }
function isDialog(o: string) { return /^- (alert|dialog|file chooser)/.test(o) }
// a dialog AT-SPI maps before its children answers with its root alone; read it again until the children show
async function filled(w: Win, kept?: Kept) {
  let o = await outline(w, {}, kept, Infinity)
  for (let i = 0; i < 20 && isDialog(o) && !o.trim().includes('\n'); i++) { await sleep(50); o = await outline(w, {}, undefined, Infinity) }
  return o
}

function plus(a: desk.Usage = {}, b: desk.Usage = {}): desk.Usage {
  const out: desk.Usage = structuredClone(a)
  for (const [k, v] of Object.entries(b)) {
    const u = out[k] ??= { calls: 0, input_tokens: 0, output_tokens: 0, cached_tokens: 0 }
    u.calls += v.calls; u.input_tokens += v.input_tokens; u.output_tokens += v.output_tokens; u.cached_tokens += v.cached_tokens
  }
  return out
}

async function decide(state: unknown, q: Record<string, desk.Question>) {
  const t0 = Date.now()
  try { return await desk.decide(state, q) } finally { tick('jev', t0); T.jev_calls = (T.jev_calls ?? 0) + 1 }
}

async function vision(prompt: string, shot: Buffer): Promise<Vision> {
  const t0 = Date.now()
  try {
    const a = await desk.ask(`${prompt}\nAnswer with one JSON object and nothing else.`, [shot])
    return JSON.parse(a.slice(a.indexOf('{'), a.lastIndexOf('}') + 1)) as Vision
  } finally { tick('vision', t0) }
}

async function checked(n: Node): Promise<boolean | 'mixed' | null> {
  const t0 = Date.now()
  try { const r = await desk.read(n.id); return r.states.includes('mixed') ? 'mixed' : r.states.includes('checked') || r.states.includes('pressed') } catch { return null } finally { tick('state', t0) }
}
// a command-line word may extend the app name: gnome-terminal-server for gnome-terminal
function ownedBy(pid: number | null, app: string) {
  if (OS !== 'linux' || !pid) return false
  try { return readFileSync(`/proc/${pid}/cmdline`, 'utf8').toLowerCase().split(/[\0/\s]+/).some((x) => x.startsWith(app)) } catch { return false }
}
// the names of the desktop entries a launcher script such as localc (`soffice --calc "$@"`) stands for: entries run with its arguments
function entryNames(app: string): string[] {
  if (OS !== 'linux') return []
  try {
    const path = (process.env.PATH ?? '').split(':').map((d) => `${d}/${app}`).find((p) => existsSync(p))
    if (!path || statSync(path).size > 2048) return []
    const body = readFileSync(path, 'utf8')
    const args = body.startsWith('#!') ? (body.trim().split('\n').at(-1) ?? '').split(/\s+/).slice(1).filter((a) => !a.startsWith('"$')).join(' ') : ''
    if (!args) return []
    const dir = '/usr/share/applications'
    return readdirSync(dir).filter((f) => f.endsWith('.desktop')).flatMap((f) => {
      const d = readFileSync(`${dir}/${f}`, 'utf8')
      const exec = /^Exec=(.*)$/m.exec(d)?.[1]?.split(/\s+/).slice(1).filter((a) => !a.startsWith('%')).join(' ')
      const name = /^Name=(.*)$/m.exec(d)?.[1]
      return name && exec === args ? [norm(name)] : []
    })
  } catch { return [] }
}
function gdbus(dest: string, path: string, method: string, ...args: string[]) {
  return execFileSync('gdbus', ['call', '--session', '--dest', dest, '--object-path', path, '--method', method, ...args], { encoding: 'utf8', timeout: 2000 })
}
// a GtkApplication menu toggle: its label maps to an action in org.gtk.Menus, whose state org.gtk.Actions reports
function gtkToggle(pid: number, label: string): boolean | null {
  if (OS !== 'linux') return null
  const t0 = Date.now()
  try {
    const comm = readFileSync(`/proc/${pid}/comm`, 'utf8').trim().toLowerCase()
    busNames ??= [...gdbus('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.ListNames').matchAll(/'([\w.-]+)'/g)].map((x) => x[1] ?? '')
    for (const bus of busNames.filter((n) => n.toLowerCase().split('.').at(-1) === comm)) {
      const root = `/${bus.replace(/\./g, '/')}`
      const listing = gdbus(bus, `${root}/menus/menubar`, 'org.gtk.Menus.Start', `[${[...Array(64).keys()].join(',')}]`)
      const actions = [...listing.matchAll(/\{[^{}]*\}/g)].map((x) => x[0]).filter((d) => norm(/'label': <'((?:[^'\\]|\\.)*)'>/.exec(d)?.[1] ?? '') === norm(label))
        .map((d) => /'action': <'([^']+)'>/.exec(d)?.[1]).filter((a): a is string => !!a)
      if (new Set(actions).size !== 1) continue
      const [scope, ...rest] = (actions[0] ?? '').split('.'); const action = rest.join('.')
      const paths = scope === 'app' ? [root] : [...gdbus(bus, `${root}/window`, 'org.freedesktop.DBus.Introspectable.Introspect').matchAll(/<node name="(\d+)"/g)].map((x) => `${root}/window/${x[1]}`)
      const states = new Set(paths.map((p) => /\[<(true|false)>\]/.exec(gdbus(bus, p, 'org.gtk.Actions.Describe', action))?.[1]))
      if (states.size === 1 && !states.has(undefined)) return states.has('true')
    }
    return null
  } catch { return null } finally { tick('state', t0) }
}

async function windowFor(want: string | undefined, ms = 3000, saw?: string): Promise<Win> {
  const t0 = Date.now()
  if (!want) {
    const ws = await current(); let w = frontOf(ws)
    if (BG && !w) { tick('wait', t0); throw new Surprise('background gui_plan has no target window: start the plan with an app step, or name the window with a wait step') }
    // an untitled focused window that is no alert (a completion popup) leaves the step in the window the plan was in
    if (w && !w.title.trim() && lastSeen && !isAlert(await outline(w))) { const pid = w.pid; w = ws.find((x) => x.title === lastSeen && x.pid === pid) ?? w }
    tick('wait', t0); if (!w) throw new Surprise('no focused window', {}, true); return w
  }
  const end = Date.now() + ms; let asked: string | null = null
  for (;;) {
    const ws = await desk.windows(); const w = frontOf(ws)
    if (w && (norm(w.title).includes(norm(want)) || (saw && w.title === saw))) return found(w, t0)
    if (BG) {
      const named = (x: Win) => norm(x.title).includes(norm(want)) || (!!saw && x.title === saw)
      const t = target
      const bg = ws.find((x) => named(x) && x.pid === t?.pid) ?? ws.find(named)
      if (bg) { follow(bg, ws); return found(bg, t0) }
    } else if (w && known.has(w.title) && Date.now() - t0 > 150) {
      const bg = ws.find((x) => norm(x.title).includes(norm(want)))
      if (bg) { await desk.focus(bg.title, bg.pid); return found(bg, t0) }
    }
    if (w && !known.has(w.title) && w.title !== asked && Date.now() - t0 > 250) {
      asked = w.title
      const o = await outline(w)
      if (blocked(o)) { tick('wait', t0); throw new Surprise(unanswered(w.title), await seen()) }
      if (isAlert(o)) { tick('wait', t0); throw new Surprise(`alert "${w.title}" opened instead of "${want}"`, await seen()) }
      const d = await decide({ window_title: w.title, contents: o.slice(0, 800) }, { is: desk.yes(`The plan expects the "${want}" window in front next. Is this window that one, and not a confirmation, error or other unexpected dialog?`) })
      if (prob(d.is) >= 0.7) return found(w, t0)
      if (prob(d.is) < 0.3) { tick('wait', t0); throw new Surprise(`window "${w.title}" opened instead of "${want}"`, await seen()) }
    }
    if (Date.now() > end) { tick('wait', t0); throw new Surprise(`expected window "${want}" did not appear`, await seen()) }
    await sleep(30)
  }
}
function found(w: Win, t0: number): Win { known.add(w.title); tick('wait', t0); return w }

function named(nodes: Node[], i: number) { for (const [, a] of up(nodes, nodes[i]?.parent ?? -1)) if (a.name) return a.name; return '' }
function sibling(nodes: Node[], i: number) { return nodes.slice(Math.max(0, i - 2), i + 3).filter((x) => x.role === 'text' && x.parent === nodes[i]?.parent && x.name).map((x) => x.name)[0] }
function fingerprint(nodes: Node[], h: { n: Node; i: number }): Fp {
  const fp: Fp = { role: h.n.role, name: h.n.name, parent: named(nodes, h.i), nth: 0 }
  const label = sibling(nodes, h.i); if (label) fp.label = label
  fp.nth = matches(nodes, fp).findIndex((x) => x.i === h.i)
  return fp
}
function matches(nodes: Node[], fp: Fp, within?: number) {
  return candidates(nodes, fp.role, within).filter(({ n, i }) => n.name === fp.name && named(nodes, i) === fp.parent && (sibling(nodes, i) ?? '') === (fp.label ?? ''))
}
function byFp(nodes: Node[], fp: Fp | undefined, within?: number): Hit | null {
  if (!fp) return null
  const same = matches(nodes, fp, within)
  const hit = same.length === 1 ? same[0] : same[fp.nth]
  return hit ? { ...hit, how: 'fp' } : null
}

function candidates(nodes: Node[], role?: string, within?: number) {
  const inside = (i: number) => { if (within === undefined) return true; for (const [p] of up(nodes, nodes[i]?.parent ?? -1)) if (p === within) return true; return false }
  const item = (n: Node) => ITEMS.has(n.role) && LISTS.has(nodes[n.parent]?.role ?? '')
  const roles = role ? new Set(role.split('|')) : null
  return nodes.map((n, i) => ({ n, i })).filter(({ n, i }) => (ACTIVE.has(n.role) || item(n)) && (!roles || roles.has(n.role)) && inside(i))
}

async function resolve(nodes: Node[], target: string, role?: string, within?: number, bar = 0.5, fp?: Fp, ask = true): Promise<Hit | null> {
  const t0 = Date.now()
  const pinned = byFp(nodes, fp, within)
  if (pinned) { tick('ax', t0); return pinned }
  const pool = candidates(nodes, role, within)
  const want = norm(target)
  const covers = (n: Node) => {
    const nm = norm(n.name); if (!nm) return false
    const a = nm.split(' '), b = want.split(' ')
    return (` ${nm} `.includes(` ${want} `) && b.length * 2 >= a.length) || (a.length >= 2 && ` ${want} `.includes(` ${nm} `))
  }
  const said = role || /\b(radio|spin|option|check) button\b/.test(want) ? undefined : Object.entries(SAID).find(([w]) => want.split(' ').includes(w))?.[1]
  const core = want.split(' ').filter((w) => !FILLER.has(w)).join(' ')
  const exact = (n: Node) => norm(n.name) === want || (!!core && norm(n.name) === core)
  const inside = (n: Node, i: number) => { for (const [p] of up(nodes, n.parent)) if (p === i) return true; return false }
  for (const f of [exact, (n: Node) => covers(n) && (!said || said.includes(n.role))]) {
    const all = pool.filter(({ n }) => f(n)); const drawn = all.filter(({ n }) => n.shown && !!n.bounds)
    const some = drawn.length ? drawn : all.filter(({ n }) => n.role.startsWith('menu'))
    const hit = some.filter(({ n, i }) => !some.some((o) => o.i !== i && norm(o.n.name) === norm(n.name) && inside(o.n, i)))
    const [one, two] = hit
    if (one && !two) { tick('ax', t0); return { ...one, how: 'ax' } }
  }
  const shown = pool.filter(({ n }) => n.shown)
  tick('ax', t0)
  if (!shown.length || !ask) return null
  const words = want.split(' ')
  const ranked = shown.map((c) => { const all = norm(`${c.n.role} ${c.n.name} ${c.n.value}`); return { c, s: words.filter((x) => all.includes(x)).length } }).sort((a, b) => b.s - a.s).slice(0, 40)
  const label = (c: { n: Node; i: number }) => {
    const sib = sibling(nodes, c.i); const par = nodes[c.n.parent]?.name ?? ''
    return `${c.n.role} "${c.n.name}"${c.n.value ? ` value "${c.n.value}"` : ''}${sib ? ` labelled "${sib}"` : ''}${c.n.shortcut ? ` shortcut ${c.n.shortcut}` : ''}${par ? ` in "${par}"` : ''}`
  }
  const opts: Record<string, string> = { none: 'none of these elements' }
  for (const { c } of ranked) opts[`r${c.n.ref}`] = label(c)
  const { d, viewed } = await pick(target, ranked.map(({ c }) => c.n), opts)
  const probs = Object.entries(d.pick?.probabilities ?? {}).sort((a, b) => b[1] - a[1])
  const [top, p1] = probs[0] ?? ['none', 0]; const p2 = probs[1]?.[1] ?? 0
  const hit = ranked.find(({ c }) => `r${c.n.ref}` === top)
  if (!hit || p1 < bar) return null
  const own = terms(`${hit.c.n.name} ${hit.c.n.value} ${sibling(nodes, hit.c.i) ?? ''}`)
  if (!viewed && (!terms(target).some((w) => own.includes(w)) || (hit.c.n.role === 'tab' && !want.split(' ').includes('tab')))) return null
  if (said && !said.includes(hit.c.n.role)) return null
  if (p1 - p2 < PICK_MARGIN) {
    const near = probs.slice(0, 3).map(([k, p]) => `${k === 'none' ? 'none' : opts[k]} ${p.toFixed(2)}`).join('; ')
    throw new Surprise(`ambiguous target "${target}": ${near}; name the element more exactly`, await seen())
  }
  return { ...hit.c, how: `${viewed ? 'jev-image' : 'jev'}(${p1.toFixed(2)})`, p: p1, margin: p1 - p2 }
}

// a menu bar entry that several menu bars repeat (LibreOffice exposes its menu bar more than once) resolves in the first of them
function barMenu(nodes: Node[], name: string): Hit | null {
  const tops = nodes.map((n, i) => ({ n, i })).filter(({ n }) => n.role === 'menu' && norm(n.name) === norm(name) && nodes[n.parent]?.role === 'menubar')
  const [first, second] = tops
  return first && second ? { ...first, how: 'ax' } : null
}

// jev's pick on a screenshot, which tells apart elements of the same name by where they are; on any failure its pick on the text
async function pick(target: string, pool: Node[], opts: Record<string, string>) {
  const t0 = Date.now()
  try {
    if (BG) throw new Error(NO_PIXELS)
    const area = await desk.screen()
    const at = (n: Node) => n.bounds ? ` at (${((n.bounds[0] + n.bounds[2] / 2) / area.width).toFixed(2)}, ${((n.bounds[1] + n.bounds[3] / 2) / area.height).toFixed(2)})` : ''
    const placed: Record<string, string> = { none: opts.none ?? '' }
    for (const n of pool) placed[`r${n.ref}`] = `${opts[`r${n.ref}`]}${at(n)}`
    const list = `Elements of the focused window, with their centres as fractions of the screenshot:\n${pool.map((n) => placed[`r${n.ref}`]).join('\n')}`
    const d = await desk.look([list, desk.image(await desk.shot())], { pick: desk.choice(`Which element is "${target}"? Use the screenshot to tell apart elements with the same name.`, placed) })
    return { d, viewed: true }
  } catch {
    return { d: await desk.decide(pool.map((n) => opts[`r${n.ref}`]).join('\n'), { pick: desk.choice(`Which element is "${target}"?`, opts) }), viewed: false }
  } finally { tick('jev', t0); T.jev_calls = (T.jev_calls ?? 0) + 1 }
}

async function grounded(x: number, y: number, target: string, w: Win, area: desk.Size) {
  const t0 = Date.now()
  const [ox, oy] = [area.left ?? 0, area.top ?? 0]
  const fit = (cw: number, ch: number): [number, number] => [Math.max(ox, Math.min(ox + area.width - cw, Math.round(x - cw / 2))), Math.max(oy, Math.min(oy + area.height - ch, Math.round(y - ch / 2)))]
  const yes = async (state: unknown[], question: string) => +prob((await desk.look(state, { on: desk.yes(question) })).on).toFixed(2)
  const peek = async (png: Buffer, at: number[]) => {
    const [px, py] = at.map(Math.round)
    return yes([`A ${png.readUInt32BE(16)}x${png.readUInt32BE(20)} crop of the screen. A click is about to land on its pixel (${px}, ${py}).`, desk.image(png)], `Is the pixel at (${px}, ${py}) of the image on ${target}?`)
  }
  const [cw, ch] = [Math.min(CROP.w, area.width), Math.min(CROP.h, area.height)], [left, top] = fit(cw, ch)
  const [spot, shot] = await Promise.allSettled([desk.at(x, y, w.title, w.pid), desk.shot({ x: left - ox, y: top - oy, w: cw, h: ch })])
  const under = spot.status === 'fulfilled' ? spot.value : { found: null, path: [] }
  const what = under.found ? [under.found, ...(under.path ?? []).toReversed()].filter((n) => n.name).map((n) => `${n.role} "${n.name}"`).join(' under ') : ''
  try {
    if (shot.status === 'rejected') throw shot.reason
    const k = shot.value.readUInt32BE(16) / cw
    const p = await peek(shot.value, [(x - left) * k, (y - top) * k])
    const out: { what: string; ok: boolean; check: string; p: number; p_zoom?: number | null; p_full?: number | null } = { what, ok: p >= 0.5, check: 'jev-image', p }
    if (out.ok) return out
    const b = under.found?.bounds ?? (under.found?.id ? (await desk.read(under.found.id).catch(() => null))?.bounds : undefined)
    const el = b && b[2] > 0 && b[3] > 0 ? b : null
    const screen = unpng(await desk.shot()), s = screen.w / area.width
    const crop = async (cw: number, ch: number, k: number) => {
      const [left, top] = fit(cw, ch)
      return peek(cut(screen, Math.round((left - ox) * s), Math.round((top - oy) * s), Math.round(cw * s), Math.round(ch * s), k), [(x - left) * s * k, (y - top) * s * k])
    }
    if (!el || el[2] * el[3] < SMALL) {
      const rw = Math.min(area.width, el ? Math.min(384, Math.max(96, Math.round(1.5 * el[2]), Math.round(5 * el[3] * 4 / 3))) : 192)
      const rh = Math.min(area.height, el ? Math.round(rw * 3 / 4) : 144)
      out.check += '+zoom'; out.p_zoom = await crop(rw, rh, Math.max(1, Math.ceil(Math.max(384 / (rw * s), 288 / (rh * s))))).catch(() => null)
      out.ok = (out.p_zoom ?? 0) >= ZOOM_BAR
    }
    if (!out.ok && desk.setting('DESK_FULL_CHECK') !== 'off') {
      const [px, py] = [Math.round((x - ox) * s), Math.round((y - oy) * s)]
      const full = async () => yes([`The screen, ${screen.w}x${screen.h}. A red ring with four ticks marks the spot a click is about to land on, centred at pixel (${px}, ${py}).`, desk.image(ring(screen, px, py, Math.round(RING * s)))], `Is the spot marked by the red ring on ${target}?`)
      out.check += '+full'; out.p_full = await full().catch(() => null)
      out.ok = (out.p_full ?? 0) >= FULL_BAR
    }
    return out
  } catch (e: unknown) {
    const p = what.trim() ? prob((await decide({ element_under_point: what, window_title: w.title }, { is: desk.yes(`Is this element "${target}"?`) })).is) : 0
    return { what, ok: p >= 0.7, check: `jev (${(e instanceof Error ? e.message : String(e)).slice(0, 80)})`, p: +p.toFixed(2) }
  } finally { tick('check', t0) }
}

// a shown control takes a pointer click, since an AT-SPI action runs its handler inside the a11y bridge, where a modal dialog blocks the app
async function act(n: Node) {
  const t0 = Date.now()
  try {
    const r = await desk.press(n.id)
    if (!r.ok) throw new Surprise(`"${n.name}" took no ${r.how}`, await seen())
    if (BG && r.unverified) unheard ??= r.unverified
    return r.how
  } finally { tick('act', t0) }
}

// a menu entry with a shortcut takes the shortcut, since an AT-SPI action can open a modal dialog that deadlocks the app on the first key
async function choose(nodes: Node[], n: Node, rec: { via?: string }, open = false, w: Win | null = null) {
  const t0 = Date.now()
  try {
    const mods = n.shortcut.split('+').slice(0, -1)
    if (OS !== 'darwin' && n.shortcut && (mods.some((x) => x !== 'shift') || /^f\d+$/.test(n.shortcut))) {
      if (open) for (const _ of menus(nodes, n.parent)) await desk.key('Escape', at(w))
      try { const k = await desk.key(n.shortcut, at(w)); if (BG && k.unverified) unheard ??= k.unverified; rec.via = 'shortcut'; return } catch {}
    }
    rec.via = await act(n)
  } finally { tick('act', t0) }
}

async function settle(before: string | undefined) {
  const t0 = Date.now()
  const ws = await until((ws) => frontOf(ws)?.title !== before, 150)
  tick('settle', t0); return ws
}

async function ready(w: Win) {
  const t0 = Date.now()
  for (;;) {
    let nodes: Node[]; try { nodes = parse(await snapOf(w)) } catch { break }
    if (!nodes.filter((n) => n.depth === 1 && n.role === 'button' && n.shown).at(-1)?.disabled || Date.now() - t0 >= 400) break
    await sleep(20)
  }
  tick('ready', t0)
}

// a typed line that ends in a newline submits a file chooser: Return until it closes, since the first one may only close the completion popup
async function submit(w: Win, chooser = false) {
  if (chooser) await ready(w)
  const t0 = Date.now()
  const [title, pid] = [w.title, w.pid]
  let ws = await desk.windows()
  for (let i = 0; i < 3 && titledIn(ws, title, pid); i++) {
    await desk.key('Return', at(w))
    ws = await until((ws) => !titledIn(ws, title, pid), 600)
  }
  tick('submit', t0)
  if (titledIn(ws, title, pid) && frontOf(ws)?.title === title) throw new Surprise(`"${title}" is still open after Return`, await seen())
}

async function closed(title: string, pid: number | null) {
  const t0 = Date.now()
  const landed = (ws: Win[]) => { const f = frontOf(ws); return !!f && f.title !== title && f.pid !== null }
  const gone = await until((ws) => !titledIn(ws, title, pid), 1000)
  const ws = await until((ws) => titledIn(ws, title, pid) || landed(ws), 500, gone)
  tick('settle', t0); return !titledIn(ws, title, pid)
}

function shownFields(nodes: Node[], fp: Pick<Fp, 'role' | 'name' | 'label'>) {
  return candidates(nodes, fp.role).filter(({ n, i }) => n.name === fp.name && (sibling(nodes, i) ?? '') === (fp.label ?? '') && n.shown)
}
async function typedInto(w: Win, text: string): Promise<Fp | undefined> {
  const t0 = Date.now(); const squash = (v: string) => v.replace(/\s+/g, '')
  try {
    const nodes = parse(await snapOf(w))
    const hit = candidates(nodes, 'textbox|searchbox').find(({ n }) => n.shown && squash(n.value) === squash(text))
    if (!hit) return undefined
    const fp: Fp = { role: hit.n.role, name: hit.n.name, parent: '', nth: 0 }
    const label = sibling(nodes, hit.i); if (label) fp.label = label
    fp.nth = shownFields(nodes, fp).findIndex((x) => x.i === hit.i)
    return fp
  } catch { return undefined } finally { tick('blank', t0) }
}
// the text of each element of `w` that holds text, by id, read before and after typing the driver may not confirm
type Text = { value?: string; field: boolean }
async function texts(w: Win): Promise<Map<string, Text> | null> {
  try {
    const out = new Map<string, Text>()
    for (const n of parse(await snapOf(w))) {
      const field = n.role === 'document' || FIELD_ROLES.has(n.role)
      if (n.value) out.set(n.id, { value: n.value, field })
      else if (field) out.set(n.id, { value: (await desk.read(n.id, { text: true }).catch(() => null))?.value, field })
    }
    return out
  } catch { return null }
}
function unchanged(before: Map<string, Text> | null, after: Map<string, Text> | null) {
  if (!before || !after || before.size !== after.size) return false
  const all = [...after]
  return all.some(([, t]) => t.field) && all.every(([id, t]) => t.value !== undefined && before.get(id)?.value === t.value)
}
function shows(before: Map<string, Text> | null, after: Map<string, Text> | null, text: string) {
  const squash = (v: string) => v.replace(/\s+/g, '').toLowerCase()
  const want = squash(text)
  const count = (v: string | undefined) => want ? squash(v ?? '').split(want).length - 1 : 0
  return !!before && !!after && [...after].some(([id, t]) => count(t.value) > count(before.get(id)?.value))
}
async function holdsText(w: Win, fp: Fp) {
  const t0 = Date.now()
  try {
    const same = shownFields(parse(await snapOf(w)), fp); const hit = same.length === 1 ? same[0] : same[fp.nth]
    return !!hit && !!hit.n.value.trim()
  } catch { return false } finally { tick('blank', t0) }
}

async function documents(w: Win) {
  const t0 = Date.now()
  try {
    const ws = await desk.windows()
    const d = BG ? ownerOf(w, ws) : ws.find((x) => x.title !== w.title && x.pid === w.pid); if (!d) return null
    const ids = parse(await snapOf(d)).filter((n) => n.role === 'textbox').map((n) => n.id)
    return async () => (await Promise.all(ids.map(async (id) => { try { return (await desk.read(id, { text: true })).value ?? '' } catch { return '' } }))).join('\n')
  } catch { return null } finally { tick('verify', t0) }
}
async function replaced(nodes: Node[], read: () => Promise<string>, before: string | undefined) {
  const t0 = Date.now(); let now = await read()
  for (let i = 0; i < 10 && now === before; i++) { await sleep(30); now = await read() }
  const f = nodes.find((n) => n.role === 'textbox'); let needle = ''
  try { needle = f ? (await desk.read(f.id)).value ?? '' : '' } catch {}
  tick('verify', t0)
  if (now === before && needle && now.includes(needle)) throw new Surprise(`Replace All replaced nothing: "${needle}" is still in the document`, await seen())
}

async function setState(s: Step, hit: Hit, rec: Rec) {
  const on = s.on ?? true
  let was = await checked(hit.n)
  if (was === null) was = await cropState(hit.n)
  rec.was = was
  if (was === on) { rec.noop = true; return }
  let now = await flip(hit.n, on)
  if (now !== on && (was === 'mixed' || now === 'mixed')) now = await flip(hit.n, on)
  if (now === null) now = await cropState(hit.n, false)
  if (now !== on) throw new Surprise(`"${hit.n.name}" is still ${now === 'mixed' ? 'mixed' : now ? 'on' : 'off'}`, await seen())
  rec.confirmed = true
}

async function flip(n: Node, on: boolean) {
  await act(n)
  let now = await checked(n)
  for (let i = 0; i < 5 && now !== on && now !== null; i++) { await sleep(30); now = await checked(n) }
  return now
}

async function option(combo: Node, opts: Node[], value: string, target: string, rec: Rec) {
  const exact = opts.filter((n) => norm(n.name) === norm(value)); const near = opts.filter((n) => norm(n.name).includes(norm(value)))
  const o = exact.length === 1 ? exact[0] : near.length === 1 ? near[0] : null
  if (!o) throw new Surprise(`"${target}" has no option "${value}"; its options: ${opts.map((n) => `"${n.name}"`).join(', ')}`, await seen())
  rec.value = o.name
  if (norm(combo.name) === norm(o.name)) { rec.noop = true; return }
  const t0 = Date.now()
  await act(combo)
  for (let i = 0; i < 25 && !(await desk.read(o.id)).states.includes('showing'); i++) await sleep(20)
  await act(o); tick('act', t0)
  let got = ''
  for (let i = 0; i < 10; i++) { got = (await desk.read(combo.id)).name; if (norm(got) === norm(o.name)) { rec.confirmed = true; return } await sleep(40) }
  throw new Surprise(`"${target}" reads "${got}" after set`, await seen())
}

async function cropState(n: Node, unsent = true) {
  if (BG && (!AUTO || !unsent)) throw new Surprise(`the state of "${n.name}" needs a look at the screen: ${NO_PIXELS}`, await seen())
  if (BG) await front(frontOf(await desk.windows()))
  const b = (await desk.read(n.id)).bounds ?? n.bounds
  if (!b) throw new Error(`"${n.name}" has no screen position`)
  const shot = await desk.shot({ x: Math.max(0, b[0] - 40), y: Math.max(0, b[1] - 4), w: b[2] + 80, h: b[3] + 8 })
  return (await vision(`This is one control of a desktop app: "${n.name}". Is it switched on (a check mark, tick, filled box or pressed state)? {"on": true or false}`, shot)).on === true
}

async function run(s: Step, rec: Rec, kept?: Kept) {
  if (s.app) {
    const ws = await desk.windows(); const before = new Set(ws.map((w) => w.title))
    let launched = false; let background = false
    const name = norm(s.title ?? s.app); const word = !s.title && /^[\w.-]+$/.test(s.app)
    const entries = word ? entryNames(s.app) : []
    const app = norm(s.app)
    const pkg = !s.title && ANDROID_PACKAGE.test(s.app)
    const titled = (w: Win) => pkg ? w.title === s.app || w.title.endsWith(` - ${s.app}`) : norm(w.title).includes(name)
    const own = (w: Win) => titled(w) || (!s.title && !!w.app && norm(w.app).includes(app)) || (word && ((!pkg && norm(w.title).includes(name.replace(/-/g, ' '))) || ownedBy(w.pid, name) || entries.some((e) => norm(w.title).includes(e))))
    const has = async () => { const ws = await desk.windows(); return ws.find(own) ?? (launched ? ws.find((w) => w.title && !before.has(w.title)) : undefined) }
    let w = (BG ? ws.find((x) => own(x) && !!x.title.trim()) : undefined) ?? ws.find(own)
    const listed = !w && word && !BG ? await desk.focus(s.app).catch(() => null) : null
    if (listed) { known.add(listed.title); rec.window = lastSeen = listed.title; return }
    if (!w) {
      const t0 = Date.now(); launched = true
      background = (await desk.launch(s.app)).background === true
      for (const end = Date.now() + 8000; !(w = await has()); await sleep(50)) {
        if (Date.now() > end) throw new Surprise(`app "${s.app}" has no window`, await seen())
      }
      tick('launch', t0)
    }
    if (BG) { follow(w, await desk.windows()); known.add(w.title); rec.window = lastSeen = w.title; return }
    if (background || !w.active) { const t1 = Date.now(); const f = await desk.focus(w.title, w.pid); tick('focus', t1); if (f.path) rec.focused = `${f.path} front ${f.front ?? '-'}` }
    known.add(w.title); rec.window = lastSeen = w.title; await windowFor(w.title); return
  }
  if (s.type !== undefined) {
    const nl = s.type.endsWith('\n'); const ws = await current(nl ? OPENING : SETTLE); const w = frontOf(ws); const t0 = Date.now()
    if (BG && !w) throw new Surprise('background gui_plan has no target window to type into: start the plan with an app step, or name the window with a wait step')
    const dialog = !s.type.includes('\n') && !!w && ws.some((x) => x.title !== w.title && x.pid === w.pid)
    const o = (nl || dialog) && w ? await filled(w, kept) : ''
    if (w && blocked(o)) throw new Surprise(unanswered(w.title), await seen())
    const chooser = isChooser(o); const sub = chooser && nl
    const first = !typed; typed = true
    const was = BG && w ? await texts(w) : null
    if (s.blank && w && await holdsText(w, s.blank)) throw new Surprise(`"${w.title}" already holds text where this flow typed into an empty field, and typing would add to it: open a new document (for example {key: "ctrl+n"}) or empty the field, then replay the flow`, await seen())
    // GTK preselects the name up to its last dot: a bare name keeps the extension, a path or a name with its own extension replaces the whole field
    if (chooser && /[/\\]|\.[a-z][a-z0-9]{0,7}\n?$/i.test(s.type)) await selectLine(w)
    // a line break goes as Return, which GTK text views take where they ignore a Linefeed
    for (const [i, part] of (sub ? s.type.slice(0, -1) : s.type).split(/\r?\n/).entries()) { if (i) heard(await desk.key('Return', at(w)), rec); if (part) heard(await desk.typeText(part, at(w)), rec) }
    tick('act', t0)
    if (BG && rec.unverified && w && !sub) {
      const now = await texts(w)
      if (AUTO && !shows(was, now, s.type) && unchanged(was, now) && !taken(await desk.windows())) {
        await desk.focus(w.title, w.pid)
        for (const [i, part] of s.type.split(/\r?\n/).entries()) { if (i) await desk.key('Return'); if (part) await desk.typeText(part) }
        rec.how = 'foreground'; rec.unverified = undefined; fronted = true; BG = false
      } else if (!shows(was, now, s.type)) throw new Surprise(`"${s.type}" did not show in "${w.title}" after typing: ${rec.unverified}`, await seen())
      rec.confirmed = true
    }
    if (recording && first && !chooser && w) rec.blank = await typedInto(w, s.type)
    const v = nl && !sub ? await focused() : null
    if (sub && w) { rec.submit = true; await submit(w, true) } else if (v && isDialog(v.title === w?.title ? o : await outline(v)) && await closed(v.title, v.pid)) rec.submit = true
    if (dialog && w) {
      const want = s.type.slice(0, 30).replace(/\\/g, '\\\\').replace(/"/g, '\\"'); let after = ''
      for (let k = 0; k < 5; k++) { after = await outline(w); if (!isDialog(after) || after.split('\n').some((l) => /^\s*- (textbox|searchbox|combobox|spinbutton)\b.*: ".*/.test(l) && l.includes(want))) return; await sleep(40) }
      throw new Surprise(`"${s.type}" reached no text field of "${w.title}" (Tab can land on a button): fill the field with {set: label, value}`, await seen())
    }
    return
  }
  if (s.key) {
    const w = await focused()
    if (BG && !w) throw new Surprise('background gui_plan has no target window for the key: start the plan with an app step, or name the window with a wait step')
    const o = /^(enter|return)$/i.test(s.key) && w ? await filled(w, kept) : ''
    if (w && blocked(o)) throw new Surprise(unanswered(w.title), await seen())
    if (w && isChooser(o)) { rec.submit = true; await submit(w, true); return }
    const t0 = Date.now(); heard(await desk.key(s.key, at(w)), rec); tick('act', t0)
    pressed = w && /\b(ctrl|control|alt|cmd|command|meta|super)\+|^f\d+$/i.test(s.key) ? { title: w.title, at: Date.now() } : null
    if (w && isDialog(o) && await closed(w.title, w.pid)) rec.submit = true
    return
  }
  if (s.wait) { const w = await windowFor(s.wait, s.timeout_ms ?? 5000, s.seen); rec.seen = lastSeen = w.title; return }
  if (s.close) {
    // close shuts a dialog: the named window, else the focused dialog, else a dialog of the focused app; never an app's main window unless named
    const t0 = Date.now(); const ws = s.window ? [] : await desk.windows(); let w: Win | null = s.window ? await windowFor(s.window, 3000, s.seen) : frontOf(ws)
    const o = w && !s.window ? await outline(w, {}, kept) : ''
    const self = w
    const others = self ? ws.filter((x) => x.pid === self.pid && x.title !== self.title && (!BG || ours(x, ws))) : []
    if (w && blocked(o) && others.length) { rec.seen = w.title; rec.how = 'wm'; unique(w); await desk.close(w.title, w.pid); await settle(w.title); tick('act', t0); return }
    if (w && blocked(o)) throw new Surprise(unanswered(w.title), await seen())
    if (w && !s.window && !isDialog(o)) {
      let d: Win | null = null; for (const x of others) if (isDialog(await outline(x))) { d = x; break }
      const pid = w.pid
      if (BG && !d && OS === 'darwin' && ws.some((x) => x.pid === pid && x.owner === undefined && x.role !== 'window' && !adopted.has(x.id))) throw new Surprise(`a dialog of "${w.title}"'s app has no owner the OS names, and background close never guesses whether it belongs to the plan's window: name it with {close: true, window}`, await seen())
      w = d
    }
    if (!w) { rec.noop = true; tick('act', t0); return }
    const [title, pid] = [w.title, w.pid]; rec.seen = title; if (!w.active && !BG) await desk.focus(title, pid); await desk.key('Escape', at(w))
    if (titledIn(await settle(w.title), title, pid)) { unique(w); await desk.close(title, pid) }
    tick('act', t0); await closed(title, pid); return
  }
  const w = await windowFor(s.window, 3000, s.seen)
  rec.seen = w.title; if (w.title.trim()) lastSeen = w.title
  let t0 = Date.now(); let nodes: Node[]
  const r = reads(s)
  try { nodes = reuse(kept, w, r)?.nodes ?? parse(await snapOf(w, r)) } catch (e: unknown) {
    if (blocked(String(e))) throw new Surprise(unanswered(w.title), await seen())
    throw e
  }
  tick('snap', t0)
  const aim = s.verify ?? s.click ?? s.check
  if (OS === 'darwin' && !r.menus && aim !== undefined) {
    const near = await resolve(nodes, aim, targetRole(s), undefined, 0.7, s.fp, false)
    if (!near || (near.how !== 'fp' && norm(near.n.name) !== norm(aim))) nodes = parse(await snapOf(w, { ...r, menus: true }))
  }
  if (s.expect) {
    const d = await decide({ window_title: w.title, contents: render(nodes, 60, false, 4000) }, { ok: desk.yes(s.expect) })
    rec.p = +prob(d.ok).toFixed(2)
    if (prob(d.ok) >= 0.5) return
    if (BG && !AUTO) throw new Surprise(`expectation not met in the window's tree: ${s.expect}; ${NO_PIXELS}`, await seen())
    if (BG) await front(w)
    const v = await vision(`You check the state of a computer screen.\nCondition: ${s.expect}\n{"ok": true or false, "why": "one short sentence"}`, await desk.shot()); rec.verdict = v
    if (!v.ok) throw new Surprise(`expectation not met: ${s.expect} (${v.why})`, await seen())
    return
  }
  if (s.verify) {
    const hit = await resolve(nodes, s.verify, targetRole(s), undefined, 0.7, s.fp)
    if (!hit) { if (s.on === false || s.enabled === false) return; throw new Surprise(`verify: no element matches "${s.verify}"`, await seen()) }
    rec.how = hit.how; rec.fp = fingerprint(nodes, hit)
    const bad: string[] = []
    if (s.value !== undefined) { let v = ''; try { v = (await desk.read(hit.n.id)).value ?? '' } catch (e: unknown) { if (blocked(String(e))) throw new Surprise(unanswered(w.title), await seen()); v = hit.n.value }; if (!norm(v).includes(norm(s.value))) bad.push(`reads "${v}"`) }
    if (s.enabled !== undefined && hit.n.disabled === s.enabled) bad.push(hit.n.disabled ? 'is disabled' : 'is enabled')
    if (s.on !== undefined) { const c = (w.pid === null ? null : gtkToggle(w.pid, hit.n.name)) ?? (await checked(hit.n)) ?? await cropState(hit.n); rec.was = c; if (c !== s.on) bad.push(c === 'mixed' ? 'is mixed' : c ? 'is on' : 'is off') }
    if (bad.length) throw new Surprise(`verify: "${hit.n.name || s.verify}" ${bad.join(', ')}`, await seen())
    return
  }
  if (s.menu) {
    if (BG && OS === 'darwin' && !AUTO) throw new Surprise(`the macOS menu bar belongs to the front app, and an app behind it runs no menu entry: background gui_plan cannot run "${s.menu}" in "${w.title}"`, await seen())
    if (BG && OS === 'darwin') { await front(w); nodes = parse(await snapOf(w, r)) }
    const entry = await menuEntry(w, nodes, s.menu, s.fp, rec)
    const hit = entry.hit; nodes = entry.nodes
    rec.fp = fingerprint(nodes, hit)
    if (hit.p !== undefined) rec.pick = [hit.p, hit.margin ?? 0]
    const pid = w.pid
    if (s.on === undefined) { if (rec.opened) rec.via = await act(hit.n); else await choose(nodes, hit.n, rec, false, w); await settle(w.title); const now = pid === null ? null : gtkToggle(pid, hit.n.name); if (now !== null) rec.now = now; return }
    const was = pid === null ? null : gtkToggle(pid, hit.n.name)
    if (pid !== null && was !== null) {
      rec.was = was; rec.state = 'gtk'
      if (was === s.on) { rec.noop = true; return }
      await act(hit.n)
      let now = gtkToggle(pid, hit.n.name)
      for (let i = 0; i < 5 && now !== s.on; i++) { await sleep(30); now = gtkToggle(pid, hit.n.name) }
      if (now !== s.on) throw new Surprise(`menu toggle "${hit.n.name}" is still ${now ? 'on' : 'off'}`, await seen())
      rec.confirmed = true; return
    }
    if ((hit.n.role === 'menuitemcheckbox' || hit.n.role === 'menuitemradio') && OS !== 'darwin') {
      const now = await checked(hit.n); rec.was = now; rec.state = 'atspi'
      if (now === s.on) { for (let k = 0; k < (rec.opened ?? 0); k++) await desk.key('Escape', at(w)); rec.noop = true; return }
      if (now !== null) { await act(hit.n); await settle(w.title); return }
    }
    const chain = menus(nodes, hit.n.parent).reverse()
    if (!chain.length) throw new Surprise(`"${hit.n.name}" is not inside a menu`, await seen())
    if (OS === 'darwin') {
      const path = [...chain.map((q) => q.name), hit.n.name]
      const locate = (ns: Node[], upto: number): Node | undefined => {
        let at = -1
        for (let k = 0; k <= upto; k++) {
          const want = norm(path[k] ?? '')
          at = ns.findIndex((x) => norm(x.name) === want && (k === 0 ? x.role === 'menu' && ns[x.parent]?.role === 'menubar' : [...up(ns, x.parent)].some(([a]) => a === at)))
          if (at < 0) return undefined
        }
        return ns[at]
      }
      const fresh = async () => {
        for (let t = 0; ; t++) {
          try { return parse(await snapOf(w, { menus: true })) } catch (e: unknown) { if (t >= 4 || !String(e).includes('window list is stale')) throw e; await sleep(150) }
        }
      }
      const shut = async () => { for (const _ of chain) await desk.key('Escape', at(w)) }
      let cur = nodes
      const open = async () => {
        for (let k = 0; k < chain.length; k++) {
          const q = locate(cur, k); if (!q) throw new Surprise(`menu "${path[k]}" not found`, await seen())
          await act(q); await sleep(120)
        }
      }
      const read = async () => {
        try { await open() } catch (e: unknown) { if (!String(e).includes('is gone')) throw e; await shut(); cur = await fresh(); await open() }
        await shut(); cur = await fresh()
        const leaf = locate(cur, chain.length); if (!leaf) throw new Surprise(`menu entry "${hit.n.name}" not found`, await seen())
        return { leaf, on: leaf.checked }
      }
      const was = await read(); rec.was = was.on; rec.state = 'ax-validated'
      if (was.on === s.on) { rec.noop = true; return }
      await act(was.leaf); await settle(w.title)
      return
    }
    for (const q of chain) { await act(q); await sleep(120) }
    const on = await cropState(hit.n, false); rec.was = on; rec.state = 'vision'
    if (on === s.on) { for (const _ of chain) await desk.key('Escape', at(w)); rec.noop = true; return }
    await act(hit.n); await settle(w.title); return
  }
  const target = s.click ?? s.check ?? s.set ?? ''
  const hit = await resolve(nodes, target, targetRole(s), undefined, s.set ? 0.5 : 0.7, s.fp)
  if (!hit) {
    if (!s.click) throw new Surprise(`no element matches "${target}"`, await seen(), true)
    if (BG && !AUTO) throw new Surprise(`no element of "${w.title}" matches "${target}", and finding it on the screen is out: ${NO_PIXELS}`, await seen(), true)
    if (BG) await front(w)
    t0 = Date.now()
    const area = await desk.screen()
    const [x, y] = await desk.ground(await desk.shot(), `${target} in the "${w.title}" window`, area)
    tick('ground', t0)
    const { what, ok, check, p, p_zoom, p_full } = await grounded(x, y, target, w, area)
    rec.ground_hit = what.slice(0, 80); rec.checked = check; rec.p = p; rec.p_zoom = p_zoom; rec.p_full = p_full
    if (!ok) throw new Surprise(`no element matches "${target}"${what.trim() ? `; grounding pointed at ${what.slice(0, 80)}` : ''}`, await seen(), true)
    t0 = Date.now(); await desk.click(x, y); tick('act', t0)
    rec.how = 'ground'; rec.unsaved = true; await settle(w.title); return
  }
  rec.how = hit.how; rec.ref = `${hit.n.role} "${hit.n.name}"`; rec.fp = fingerprint(nodes, hit)
  if (hit.p !== undefined) rec.pick = [+hit.p.toFixed(2), +(hit.margin ?? 0).toFixed(2)]
  const bar = BG && OS === 'darwin' && !!s.click && hit.n.role === 'menuitem' && [...up(nodes, hit.n.parent)].some(([, a]) => a.role === 'menubar')
  if (bar && !AUTO) throw new Surprise(`the macOS menu bar belongs to the front app, and an app behind it runs no menu entry: background gui_plan cannot click "${hit.n.name}" in "${w.title}"`, await seen())
  if (bar) await front(w)
  if (hit.n.disabled) {
    t0 = Date.now(); let on = false
    for (let i = 0; i < 15 && !on; i++) { await sleep(40); on = !(await desk.read(hit.n.id)).states.includes('disabled') }
    tick('enable', t0)
    if (!on) throw new Surprise(`${rec.ref} is disabled`, await seen())
  }
  if (s.click && hit.how !== 'fp' && norm(hit.n.name) !== norm(target) && TOGGLE_ROLES.has(hit.n.role))
    throw new Surprise(`"${target}" is closest to ${hit.n.role} "${hit.n.name}" (${hit.how}), and a click would flip it; use {check: "${hit.n.name}", on: ...} for it, or name the element you mean exactly`, await seen())
  if (s.click && hit.n.role === 'menuitem') {
    await choose(nodes, hit.n, rec, true, w); await settle(w.title); return
  }
  if (s.click) {
    const read = norm(hit.n.name) === 'replace all' ? await documents(w) : null; const before = await read?.()
    await act(hit.n)
    if (read) await replaced(nodes, read, before); else await settle(w.title)
    return
  }
  if (s.check) { await setState(s, hit, rec); return }
  const raw = s.value
  if (raw === undefined) throw new Surprise(`set "${target}" needs a value`, await seen())
  const value = raw.endsWith('\n') ? raw.slice(0, -1) : raw
  const plain = hit.n.role === 'textbox' || hit.n.role === 'searchbox'
  const same = (v: string | undefined) => v !== undefined && (norm(v) === norm(value) || (!plain && !!norm(value) && norm(v).startsWith(norm(value))))
  const f = hit.n.role === 'combobox' ? nodes.find((x) => x.parent === hit.i && x.role === 'textbox') ?? hit.n : hit.n
  const opts = f === hit.n && hit.n.role === 'combobox' ? candidates(nodes, 'menuitem|option', hit.i) : []
  if (opts.length) { await option(hit.n, opts.map(({ n }) => n), value, target, rec); return }
  const lines = ((await desk.read(f.id, { text: true })).value ?? '').includes('\n')
  if (hit.how.startsWith('jev') && lines) throw new Surprise(`"${target}" is closest to a multi-line text (${hit.how}), and set would replace all of it: wait for the window with that field, or name it exactly`, await seen())
  t0 = Date.now()
  if (!(await desk.act(f.id, 'focus')).ok) throw new Surprise(`"${target}" takes no focus, and set would type into another field`, await seen())
  if (f.role === 'textbox' && !lines) await selectLine(w); else await desk.key(SELECT_ALL, at(w)); if (value) await desk.typeText(value, at(w)); else await desk.key('delete', at(w)); if (hit.n.role === 'spinbutton' || hit.n.role === 'slider') { await desk.key('tab', at(w)); await desk.key('shift+tab', at(w)) }
  tick('act', t0)
  t0 = Date.now(); let got: string | undefined
  for (let i = 0; i < 10; i++) { try { got = (await desk.read(f.id)).value ?? '' } catch (e: unknown) { if (blocked(String(e))) throw new Surprise(unanswered(w.title), await seen()); got = undefined }; if (same(got)) break; await sleep(40) }
  tick('verify', t0); rec.value = got
  if (!same(got)) throw new Surprise(`"${target}" reads "${got}" after set`, await seen())
  rec.confirmed = true
  if (raw.endsWith('\n')) { rec.submit = true; await submit(w, isChooser(render(nodes, 100, false))) }
}

async function menuEntry(w: Win, nodes: Node[], menu: string, fp: Fp | undefined, rec: Rec): Promise<{ hit: Hit; nodes: Node[] }> {
  const pinned = byFp(nodes, fp)
  if (pinned) { rec.how = 'fp'; return { hit: pinned, nodes } }
  const parts = menu.split('>').map((x) => x.trim())
  const soft = async (target: string, role: string | undefined, inside?: number) => { try { return await resolve(nodes, target, role, inside, 0.7) } catch (e: unknown) { if (e instanceof Surprise) return null; throw e } }
  let within: number | undefined
  let hit: Hit | null = null
  for (const [k, part] of parts.entries()) {
    const prev = hit
    // a Windows menu lists its entries only while it is open
    if (OS === 'win32' && prev?.n.role === 'menu' && !nodes.some((x) => x.parent === prev.i)) {
      await act(prev.n); rec.opened = (rec.opened ?? 0) + 1; within = undefined
      for (let t = 0; t < 10; t++) { nodes = parse(await snapOf(w, { keys: true, menus: true })); if (candidates(nodes).some(({ n }) => norm(n.name) === norm(part))) break; await sleep(30) }
    }
    const role = k < parts.length - 1 ? 'menu' : undefined
    hit = (k === 0 && role ? barMenu(nodes, part) : null) ?? await soft(part, role, within) ?? (role ? await soft(part, undefined, within) : null)
    if (!hit) {
      const leaf = await resolve(nodes, parts.at(-1) ?? part, 'menuitem|menu|menuitemcheckbox|menuitemradio')
      if (leaf?.how !== 'ax') throw new Surprise(`menu entry "${part}" not found`, await seen())
      rec.how = 'ax-anywhere'; rec.path = menus(nodes, leaf.n.parent).map((q) => q.name).reverse()
      return { hit: leaf, nodes }
    }
    within = hit.i; rec.how = hit.how
    if (k === parts.length - 1) return { hit, nodes }
  }
  throw new Surprise(`menu "${menu}" names no entry`, await seen())
}

function fill(s: Step, p: Record<string, string>): Step {
  const sub = (v: string) => v.replace(/\{\{(\w+)(:base)?\}\}/g, (all: string, n: string, b: string | undefined) => { const x = p[n]; return x === undefined ? all : b ? win32.basename(x) : x })
  return Object.fromEntries(Object.entries(s).map(([k, v]) => [k, typeof v === 'string' ? sub(v) : v])) as Step
}

function templated(steps: Step[], params: Record<string, string>) {
  const p = { ...params }
  const out = steps.map((s) => {
    const t = { ...s }
    for (const k of ['value', 'type'] as const) {
      const v = t[k]; if (v === undefined || /\{\{\w+\}\}/.test(v) || !v.replace(/\n$/, '')) continue
      const body = v.replace(/\n$/, ''); const have = Object.entries(p).find(([, x]) => x === body)
      let name = have?.[0] ?? (slug(k === 'value' ? t.set ?? 'value' : 'text').replace(/-/g, '_') || 'value')
      for (let n = 2; !have && name in p; n++) name = `${name.replace(/_\d+$/, '')}_${n}`
      p[name] = body; t[k] = `{{${name}}}${v.endsWith('\n') ? '\n' : ''}`
    }
    return t
  })
  for (const t of out) for (const k of ['wait', 'window'] as const) {
    let v = t[k]; if (!v) continue
    for (const [n, x] of Object.entries(p)) {
      const base = win32.basename(x)
      if (x.length > 2 && v.includes(x)) v = v.replace(x, `{{${n}}}`)
      else if (/[\\/]/.test(x) && base.length > 2 && v.includes(base)) v = v.replace(base, `{{${n}:base}}`)
    }
    t[k] = v
  }
  return { steps: out, params: p }
}

function flowsDir() {
  mkdirSync(FLOWS, { recursive: true, mode: 0o700 }); chmodSync(FLOWS, 0o700)
}

function git(...args: string[]) {
  gitWorks ??= OS !== 'darwin' || developerTools()
  if (!gitWorks) return
  try { execFileSync('git', ['-C', FLOWS, ...args], { stdio: 'ignore', timeout: 5000 }) } catch {}
}

function developerTools() {
  try { execFileSync('xcode-select', ['-p'], { stdio: 'ignore', timeout: 2000 }); return true } catch { return false }
}

function repo() {
  if (!existsSync(`${FLOWS}/.git`)) {
    git('init', '-q'); writeFileSync(`${FLOWS}/.gitignore`, '.runs.jsonl\n.pending.json\n', PRIVATE)
    git('config', 'user.name', 'gui_plan'); git('config', 'user.email', 'gui_plan@localhost')
  }
}

function writeIndex() {
  const rows = readdirSync(FLOWS).filter((x) => x.endsWith('.json') && !x.startsWith('.')).map((x) => { try { return JSON.parse(readFileSync(`${FLOWS}/${x}`, 'utf8')) as Flow } catch { return null } })
    .filter((x): x is Flow => !!x?.name).sort((a, b) => b.saved_at.localeCompare(a.saved_at))
  writeFileSync(INDEX, `${rows.map((x) => `${x.name}(${Object.keys(x.params).join(', ')})${x.idempotent ? '' : ' once'}: ${summary(x.steps)}`).join('\n')}\n`, PRIVATE)
}

function saveFlow(f: Flow, why: string) {
  repo()
  writeFileSync(`${FLOWS}/${f.name}.json`, `${JSON.stringify(f, null, 1)}\n`, PRIVATE)
  writeIndex()
  git('add', '-A'); git('commit', '-q', '-m', `${f.name}: ${why}`)
}

function summary(steps: Step[]) {
  const out = steps.filter((s) => !s.app && !s.wait && !s.close && !s.verify).map((s) => s.menu ?? (s.check ? `${s.check} ${s.on === false ? 'off' : 'on'}` : s.set ? `${s.set}=${s.value}` : s.type !== undefined ? `type ${s.type.replace(/\n$/, '')}` : s.key ? `key ${s.key}` : s.click))
  const text = out.join(', ')
  return text.length > 100 ? `${text.slice(0, 99)}…` : text
}

function idempotent(steps: Step[]) { return steps.every((s) => s.type === undefined && (!s.key || /^(esc|escape|tab|shift\+tab)$/i.test(s.key))) }
function bare(s: Step, p: Record<string, string>) { return JSON.stringify(Object.entries(fill(s, p)).filter(([k]) => k !== 'fp' && k !== 'seen' && k !== 'blank').sort()) }

function store(chain: Chain) {
  const tpl = templated(chain.steps, chain.params)
  const keys: Record<string, string> = { 'ctrl+n': 'new', 'ctrl+s': 'save', 'ctrl+shift+s': 'save as', 'ctrl+o': 'open' }
  const targets = chain.steps.filter((s) => s.menu || s.check || s.set || s.click || keys[s.key?.toLowerCase() ?? '']).slice(-2).map((s) => s.key ? keys[s.key.toLowerCase()] : (s.menu ?? s.check ?? s.set ?? s.click ?? '').split('>').at(-1))
  const auto = slug([chain.steps[0]?.app, ...targets].filter((x): x is string => !!x).map((x) => x.split(' ').slice(0, 3).join(' ')).join(' ')) || undefined
  if (!chain.named && auto && chain.name && auto !== chain.name) rmSync(`${FLOWS}/${chain.name}.json`, { force: true })
  if (!chain.named) chain.name = auto
  if (chain.name) saveFlow({ name: chain.name, params: tpl.params, steps: tpl.steps, idempotent: idempotent(chain.steps), runs: 1, saved_at: new Date().toISOString(), rewrites: 0, usage: chain.usage ?? {} }, `saved from ${chain.steps.length} passing steps`)
  return chain.name
}

async function promote(name: string, params: Record<string, string> = {}, rerun = false) {
  const c = pending()
  if (!c || Date.now() - c.at >= CHAIN_AGE || !(c.saved || c.escalated) || c.dirty || !c.steps.some((s) => !s.app) || !slug(name))
    return { ok: false, reason: 'nothing to save: the last gui_plan run of the last 10 minutes must start at an app step, pass, and click no target found by vision' }
  if (c.escalated || rerun) {
    const f = await focused(); const o = f ? await outline(f) : ''
    if (!f || f.title !== c.window || blocked(o) || isDialog(o)) return { ok: false, reason: `the last plan stopped at a surprise and "${f?.title ?? 'no window'}" is in front: continue it with gui_plan from the failing step, and the passing run is saved` }
  }
  const old = c.saved && !c.named ? c.name : undefined
  c.name = slug(name); c.named = true; c.params = { ...c.params, ...params }
  if (old && old !== c.name) rmSync(`${FLOWS}/${old}.json`, { force: true })
  store(c); c.saved = true; c.escalated = false
  putPending(c)
  return { ok: true, saved: c.name, params: templated(c.steps, c.params).params, ...(idempotent(c.steps) ? {} : { note: 'a replay of this flow types again' }) }
}

function pending(): Chain | null {
  try { return JSON.parse(readFileSync(PENDING, 'utf8')) as Chain | null } catch { return null }
}

function putPending(c: Chain | null) {
  try { writeFileSync(PENDING, JSON.stringify(c), PRIVATE) } catch {}
}

function loadFlow(name: string): Flow {
  const path = `${FLOWS}/${slug(name)}.json`
  if (!existsSync(path)) throw new Surprise(`no saved flow "${name}"; saved: ${existsSync(INDEX) ? readFileSync(INDEX, 'utf8').trim().split('\n').join(', ') : 'none'}`)
  return JSON.parse(readFileSync(path, 'utf8')) as Flow
}

async function find(request: string) {
  const rows = existsSync(INDEX) ? readFileSync(INDEX, 'utf8').trim().split('\n').filter(Boolean) : []
  if (!rows.length) return { ok: false, flows: [] }
  const opts: Record<string, string> = { none: 'none of these flows does it' }
  rows.forEach((r, i) => { opts[`f${i}`] = r })
  const d = await decide({ request }, { pick: desk.choice('Which saved GUI flow does this request?', opts) })
  const probs = d.pick?.probabilities ?? {}; const k = d.pick?.choice ?? 'none'; const p = probs[k] ?? 0
  const next = Object.values(probs).sort((a, b) => b - a)[1] ?? 0
  if (k === 'none' || p < 0.6 || p - next < MARGIN) return { ok: false, flow: null, p: +p.toFixed(2) }
  return { ok: true, flow: (rows[Number(k.slice(1))] ?? '').replace(/\(.*$/, ''), p: +p.toFixed(2) }
}

// a LibreOffice toolbar docked in its frame draws a menu bar's height below its extents until the frame first relays out
function drawnShift(nodes: Node[], i: number): number | null {
  const docked = (j: number) => nodes[j]?.depth === 2 && (nodes[j]?.role === 'toolbar' || nodes[j]?.role === 'tablist')
  let top = i; for (const [p, n] of up(nodes, i)) { top = p; if (n.depth <= 2 || n.parent < 0) break }
  if (!docked(top) || !nodes.some((n) => n.role === 'menubar')) return 0
  const bar = nodes.map((n, j) => n.role === 'menubar' ? nodes.flatMap((c) => c.parent === j && c.bounds ? [c.bounds] : []) : []).filter((m) => m.length)
    .map((m) => ({ y: Math.min(...m.map((b) => b[1])), h: Math.max(...m.map((b) => b[3])) })).sort((a, b) => b.h - a.h)[0]
  const rows = nodes.flatMap((n, j) => n.role === 'toolbar' && docked(j) ? nodes.flatMap((c) => c.parent === j && c.bounds && c.shown ? [c.bounds[1]] : []) : [])
  if (!bar || !rows.length) return null
  return Math.min(...rows) < bar.y + bar.h / 2 ? bar.h : 0
}

async function locate(target: string, press: boolean) {
  const calls = desk.served.length
  const out = await located(target, press)
  return desk.served.length > calls ? { ...out, served: desk.served.slice(calls) } : out
}

async function located(target: string, press: boolean) {
  const t0 = Date.now()
  const w = await focused()
  if (!w) return { found: false, why: 'no focused window', ms: Date.now() - t0 }
  let nodes = parse(await desk.snapshot(w.title, { pid: w.pid, bounds: true, menus: true }))
  let hit: Hit | null = null
  let at = w
  try {
    hit = await resolve(nodes, target, undefined, undefined, 0.7)
    const shell = OS === 'linux' && !hit && SHELL_TARGET.test(target) ? (await desk.windows()).filter((x) => x.id !== w.id && (isShell(x) || (!x.title.trim() && x.role === 'window'))) : []
    for (const x of shell) {
      nodes = parse(await desk.snapshot(x.title, { id: x.id, bounds: true, menus: true }))
      hit = await resolve(nodes, target, undefined, undefined, 0.7)
      if (hit) { at = x; break }
    }
  } catch (e: unknown) { if (!(e instanceof Surprise)) throw e; return { found: false, why: e.reason, window: w.title, ms: Date.now() - t0 } }
  if (!hit) return { found: false, why: `no element of "${w.title}" matches`, window: w.title, ms: Date.now() - t0 }
  const out: Record<string, unknown> = { found: true, how: hit.how, role: hit.n.role, name: hit.n.name, bounds: hit.n.bounds ?? null, shown: hit.n.shown, shift: drawnShift(nodes, hit.i), window: at.title, pid: at.pid }
  if (press) {
    if (hit.n.role === 'menuitem') await choose(nodes, hit.n, out, true, w); else out.via = await act(hit.n)
    await settle(w.title)
  }
  return { ...out, ms: Date.now() - t0 }
}
