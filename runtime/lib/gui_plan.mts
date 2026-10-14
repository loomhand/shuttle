// gui_plan.mts: run a GUI plan in the box without a model turn per step, save a passing plan as a flow, replay flows.
// params: {steps, params?, save?, start?} | {save} | {flow, params?, start?} | {find} | {reindex}; result: {ok, done, ms, trace, saved?, escalate?}
import { execFileSync, spawn } from 'node:child_process'
import { appendFileSync, existsSync, mkdirSync, readdirSync, readFileSync, writeFileSync } from 'node:fs'
import { homedir } from 'node:os'
import * as desk from './desk.mts'

type Step = {
  app?: string; title?: string; menu?: string; click?: string; check?: string; on?: boolean; set?: string; value?: string
  type?: string; key?: string; wait?: string; expect?: string; verify?: string; close?: boolean; role?: string; window?: string
  timeout_ms?: number; enabled?: boolean; fp?: Fp; seen?: string; blank?: Fp
}
type Fp = { role: string; name: string; parent: string; nth: number; label?: string }
type Node = { ref: number; id: string; depth: number; role: string; name: string; value: string; disabled: boolean; parent: number; shortcut: string; checked: boolean; shown: boolean; bounds?: [number, number, number, number] }
type Hit = { n: Node; i: number; how: string; p?: number; margin?: number }
type Flow = { name: string; params: Record<string, string>; steps: Step[]; idempotent: boolean; runs: number; saved_at: string; rewrites: number; usage?: desk.Usage }
type Chain = { at: number; window: string | null; pid: number | null; name?: string; named?: boolean; params: Record<string, string>; steps: Step[]; dirty: boolean; saved?: boolean; escalated?: boolean; usage?: desk.Usage }
type Params = { steps?: Step[]; flow?: string; params?: Record<string, string>; save?: string; start?: number; find?: string; reindex?: boolean }
type Win = desk.Win

const OS = process.platform
const FLOWS = OS === 'linux' ? '/workspace/flows' : `${homedir()}/workspace/flows`
const LOG = `${FLOWS}/.runs.jsonl`
const INDEX = `${FLOWS}/INDEX`
const PENDING = `${FLOWS}/.pending.json`
const CHAIN_AGE = 10 * 60 * 1000
const ACTIVE = new Set(['button', 'checkbox', 'menuitem', 'menu', 'tab', 'textbox', 'spinbutton', 'slider', 'combobox', 'radio', 'link', 'switch', 'option', 'treeitem', 'togglebutton', 'searchbox', 'menuitemcheckbox', 'menuitemradio'])
const TOGGLES = 'checkbox|switch|togglebutton|menuitemcheckbox'
const FIELDS = 'textbox|searchbox|spinbutton|slider|combobox'
const SHOWN_ROLES = new Set([...ACTIVE, 'dialog', 'alert'])
const MARGIN = 0.25
const OPS = ['app', 'menu', 'click', 'check', 'set', 'type', 'key', 'wait', 'expect', 'verify', 'close']
const CUT = '\n(the outline holds only the start of the web page: use the browser_* tools for page content)'
const SHELL_WINDOWS = ({ linux: /^(Desktop|xfce4-panel)$/, win32: /^Program Manager$/ } as Record<string, RegExp>)[OS] ?? /^$/
const CHOOSER = ({ linux: /File Chooser Widget/, win32: /^\s*- (combobox|textbox) "File name:?"/m, darwin: /^\s*- text "Save As:"/m } as Record<string, RegExp>)[OS] ?? /$^/
const SELECT_ALL = OS === 'darwin' ? 'cmd+a' : 'ctrl+a'
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))
const norm = (s: string) => s.toLowerCase().replace(/[_…]|\.\.\.|:$/g, '').replace(/\s+/g, ' ').trim()
const slug = (s: string) => norm(s).replace(/[^a-z0-9]+/g, '-').replace(/^-|-$/g, '').slice(0, 40)
let busNames: string[] | null = null
const known = new Set<string>()
let lastSeen = ''
let recording = false
let found = false
let typed = false
let pressed: { title: string; at: number } | null = null
let T: Record<string, number> = {}
const tick = (k: string, t0: number) => { T[k] = (T[k] ?? 0) + Date.now() - t0 }

class Surprise extends Error {
  reason: string
  seen: Record<string, unknown>
  constructor(reason: string, seen: Record<string, unknown> = {}) { super(reason); this.reason = reason; this.seen = seen }
}

function parse(snap: desk.Snap): Node[] {
  return snap.nodes.map((n) => ({
    ref: n.ref, id: n.id, depth: n.depth, role: n.role, name: n.name.slice(0, 80).trim(), value: n.value ?? '', disabled: n.states.includes('disabled'), parent: n.parent,
    shortcut: n.shortcut ?? '', checked: n.states.includes('checked') || n.states.includes('pressed'), shown: n.states.includes('showing'), bounds: n.bounds,
  }))
}

const focused = () => desk.focused()
async function current() {
  const k = pressed; pressed = null; const t0 = Date.now()
  while (k && (await focused())?.title === k.title && Date.now() - k.at < 150) await sleep(10)
  tick('settle', t0); return focused()
}
const titled = async (title: string, pid: number | null) => pid !== null && (await desk.windows()).some((x) => x.title === title && x.pid === pid)
// the accessibility tree of a window; `keys` adds the key bindings of its entries
async function snapOf(w: Win, keys = false): Promise<desk.Snap> {
  return desk.snapshot(w.title, { pid: w.pid, keys })
}
// one line per element, `- role "name" [disabled] [checked] [id=ID]: "value"`; a big tree keeps only what a step can act on
function render(nodes: Node[], max: number, interactive: boolean) {
  const q = (s: string) => s.slice(0, max).replace(/\\/g, '\\\\').replace(/"/g, '\\"').replace(/\n/g, '\\n')
  return nodes.filter((n) => !interactive || n.depth === 0 || SHOWN_ROLES.has(n.role)).map((n) =>
    `${'  '.repeat(n.depth)}- ${n.role}${n.name ? ` "${q(n.name)}"` : ''}${n.disabled ? ' [disabled]' : ''}${n.checked ? ' [checked]' : ''}${n.shortcut ? ` [${n.shortcut}]` : ''}${n.id ? ` [id=${n.id}]` : ''}${n.value ? `: "${q(n.value)}"` : ''}`).join('\n')
}
async function outline(w?: Win | null, keys = false) {
  if (w === undefined) w = await focused()
  if (!w) return '(no focused window)'
  try { const s = await snapOf(w, keys); const nodes = parse(s); return render(nodes, 100, nodes.length > 60).slice(0, 1800) + (s.truncated ? CUT : '') } catch (e) { return String(e) }
}
async function seen() {
  const w = await focused()
  return { window: w?.title ?? null, windows: (await desk.windows()).map((x) => x.title).filter((t) => t && !SHELL_WINDOWS.test(t)), outline: await outline(w, true) }
}
const blocked = (o: string) => /AT-SPI peer \S+ is not responding|no accessibility answer within/.test(o)
const isChooser = (o: string) => CHOOSER.test(o)
// the whole text of the focused one-line field
const selectLine = async () => { if (OS === 'darwin') await press(SELECT_ALL); else { await press('home'); await press('shift+end') } }
const unanswered = (title: string) => `"${title}" answers no accessibility calls (a dialog opened by an accessibility action blocks its app, and a key would deadlock it): close it with {close: true}, then reopen it by its keyboard shortcut`
const isAlert = (o: string) => /^- alert/.test(o)
const isDialog = (o: string) => /^- (alert|dialog|file chooser)/.test(o)
// a dialog AT-SPI maps before its children answers with its root alone; read it again until the children show
async function filled(w: Win) {
  let o = await outline(w)
  for (let i = 0; i < 20 && isDialog(o) && !o.trim().includes('\n'); i++) { await sleep(50); o = await outline(w) }
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

async function vision(prompt: string, shot: Buffer) {
  const t0 = Date.now()
  try {
    const a = await desk.ask(`${prompt}\nAnswer with one JSON object and nothing else.`, [shot])
    return JSON.parse(a.slice(a.indexOf('{'), a.lastIndexOf('}') + 1))
  } finally { tick('vision', t0) }
}

// the checked state AT-SPI publishes for check boxes and toggles, read live
async function checked(n: Node): Promise<boolean | null> {
  const t0 = Date.now()
  try { const r = await desk.read(n.id); return r.states.includes('checked') || r.states.includes('pressed') } catch { return null } finally { tick('state', t0) }
}
// a word of the window's process command line starts with the app's command, such as gnome-terminal-server for gnome-terminal
function ownedBy(pid: number | null, app: string) {
  if (OS !== 'linux' || !pid) return false
  try { return readFileSync(`/proc/${pid}/cmdline`, 'utf8').toLowerCase().split(/[\0/\s]+/).some((x) => x.startsWith(app)) } catch { return false }
}
// a GtkApplication menu toggle: its label maps to an action in org.gtk.Menus, whose state org.gtk.Actions reports
function gdbus(dest: string, path: string, method: string, ...args: string[]) {
  return execFileSync('gdbus', ['call', '--session', '--dest', dest, '--object-path', path, '--method', method, ...args], { encoding: 'utf8', timeout: 2000 })
}
function gtkToggle(pid: number, label: string): boolean | null {
  if (OS !== 'linux') return null
  const t0 = Date.now()
  try {
    const comm = readFileSync(`/proc/${pid}/comm`, 'utf8').trim().toLowerCase()
    busNames ??= [...gdbus('org.freedesktop.DBus', '/org/freedesktop/DBus', 'org.freedesktop.DBus.ListNames').matchAll(/'([\w.-]+)'/g)].map((x) => x[1])
    for (const bus of busNames.filter((n) => n.toLowerCase().split('.').at(-1) === comm)) {
      const root = '/' + bus.replace(/\./g, '/')
      const menus = gdbus(bus, `${root}/menus/menubar`, 'org.gtk.Menus.Start', `[${[...Array(64).keys()].join(',')}]`)
      const actions = [...menus.matchAll(/\{[^{}]*\}/g)].map((x) => x[0]).filter((d) => norm(/'label': <'((?:[^'\\]|\\.)*)'>/.exec(d)?.[1] ?? '') === norm(label))
        .map((d) => /'action': <'([^']+)'>/.exec(d)?.[1]).filter((a): a is string => !!a)
      if (new Set(actions).size !== 1) continue
      const [scope, ...rest] = actions[0].split('.'); const action = rest.join('.')
      const paths = scope === 'app' ? [root] : [...gdbus(bus, `${root}/window`, 'org.freedesktop.DBus.Introspectable.Introspect').matchAll(/<node name="(\d+)"/g)].map((x) => `${root}/window/${x[1]}`)
      const states = new Set(paths.map((p) => /\[<(true|false)>\]/.exec(gdbus(bus, p, 'org.gtk.Actions.Describe', action))?.[1]))
      if (states.size === 1 && !states.has(undefined)) return states.has('true')
    }
    return null
  } catch { return null } finally { tick('state', t0) }
}

// the window the step acts in: the focused one, after waiting for one that matches `want` (title text, else a jev yes/no)
async function windowFor(want: string | undefined, ms = 3000, saw?: string) {
  const t0 = Date.now()
  let w = want ? await focused() : await current()
  if (!want) {
    // an untitled focused window that is no alert (a completion popup) leaves the step in the window the plan was in
    if (w && !w.title.trim() && lastSeen && !isAlert(await outline(w))) w = (await desk.windows()).find((x) => x.title === lastSeen && x.pid === w!.pid) ?? w
    tick('wait', t0); if (!w) throw new Surprise('no focused window'); return w
  }
  const end = Date.now() + ms; let asked: string | null = null
  for (;;) {
    w = await focused()
    if (w && (norm(w.title).includes(norm(want)) || (saw && w.title === saw))) break
    if (w && known.has(w.title) && Date.now() - t0 > 150) {
      const bg = (await desk.windows()).find((x) => norm(x.title).includes(norm(want)))
      if (bg) { await desk.focus(bg.title, bg.pid); w = bg; break }
    }
    if (w && !known.has(w.title) && w.title !== asked && Date.now() - t0 > 250) {
      asked = w.title
      const o = await outline(w)
      if (blocked(o)) { tick('wait', t0); throw new Surprise(unanswered(w.title), await seen()) }
      if (isAlert(o)) { tick('wait', t0); throw new Surprise(`alert "${w.title}" opened instead of "${want}"`, await seen()) }
      const d = await decide({ window_title: w.title, contents: o.slice(0, 800) }, { is: desk.yes(`The plan expects the "${want}" window in front next. Is this window that one, and not a confirmation, error or other unexpected dialog?`) })
      if (d.is.probability >= 0.7) break
      if (d.is.probability < 0.3) { tick('wait', t0); throw new Surprise(`window "${w.title}" opened instead of "${want}"`, await seen()) }
    }
    if (Date.now() > end) { tick('wait', t0); throw new Surprise(`expected window "${want}" did not appear`, await seen()) }
    await sleep(30)
  }
  known.add(w!.title); tick('wait', t0); return w!
}

const named = (nodes: Node[], i: number) => { for (let p = nodes[i].parent; p >= 0; p = nodes[p].parent) if (nodes[p].name) return nodes[p].name; return '' }
const sibling = (nodes: Node[], i: number) => nodes.slice(Math.max(0, i - 2), i + 3).filter((x) => x.role === 'text' && x.parent === nodes[i].parent && x.name).map((x) => x.name)[0]
function fingerprint(nodes: Node[], h: { n: Node; i: number }): Fp {
  const fp: Fp = { role: h.n.role, name: h.n.name, parent: named(nodes, h.i), nth: 0 }
  const label = sibling(nodes, h.i); if (label) fp.label = label
  fp.nth = matches(nodes, fp).findIndex((x) => x.i === h.i)
  return fp
}
const matches = (nodes: Node[], fp: Fp, within?: number) =>
  candidates(nodes, fp.role, within).filter(({ n, i }) => n.name === fp.name && named(nodes, i) === fp.parent && (sibling(nodes, i) ?? '') === (fp.label ?? ''))
function byFp(nodes: Node[], fp: Fp | undefined, within?: number): Hit | null {
  if (!fp) return null
  const same = matches(nodes, fp, within)
  const hit = same.length === 1 ? same[0] : same[fp.nth]
  return hit ? { ...hit, how: 'fp' } : null
}

function candidates(nodes: Node[], role?: string, within?: number) {
  const inside = (i: number) => { if (within === undefined) return true; for (let p = nodes[i].parent; p >= 0; p = nodes[p].parent) if (p === within) return true; return false }
  return nodes.map((n, i) => ({ n, i })).filter(({ n, i }) => ACTIVE.has(n.role) && (!role || role.split('|').includes(n.role)) && inside(i))
}

// target resolution: the saved fingerprint, exact AX name, contained AX name, then a jev choice whose top answer needs `bar` and a margin
async function resolve(nodes: Node[], target: string, role?: string, within?: number, bar = 0.5, fp?: Fp): Promise<Hit | null> {
  const t0 = Date.now()
  const pool = candidates(nodes, role, within)
  const pinned = byFp(nodes, fp, within)
  if (pinned) { tick('ax', t0); return pinned }
  const want = norm(target)
  const covers = (n: Node) => {
    const nm = norm(n.name); if (!nm) return false
    const a = nm.split(' '), b = want.split(' ')
    return (` ${nm} `.includes(` ${want} `) && b.length * 2 >= a.length) || (a.length >= 2 && ` ${want} `.includes(` ${nm} `))
  }
  for (const f of [(n: Node) => norm(n.name) === want, covers]) {
    const hit = pool.filter(({ n }) => f(n))
    if (hit.length === 1) { tick('ax', t0); return { ...hit[0], how: 'ax' } }
  }
  const shown = pool.filter(({ n }) => n.shown)
  tick('ax', t0)
  if (!shown.length) return null
  const words = want.split(' ')
  const ranked = shown.map((c) => ({ c, s: words.filter((x) => norm(`${c.n.role} ${c.n.name} ${c.n.value}`).includes(x)).length })).sort((a, b) => b.s - a.s).slice(0, 40)
  const label = (c: { n: Node; i: number }) => {
    const sib = sibling(nodes, c.i); const par = c.n.parent >= 0 ? nodes[c.n.parent].name : ''
    return `${c.n.role} "${c.n.name}"${c.n.value ? ` value "${c.n.value}"` : ''}${sib ? ` labelled "${sib}"` : ''}${c.n.shortcut ? ` shortcut ${c.n.shortcut}` : ''}${par ? ` in "${par}"` : ''}`
  }
  const opts: Record<string, string> = { none: 'none of these elements' }
  for (const { c } of ranked) opts[`r${c.n.ref}`] = label(c)
  const d = await decide(ranked.map(({ c }) => label(c)).join('\n'), { pick: desk.choice(`Which element is "${target}"?`, opts) })
  const probs = Object.entries(d.pick.probabilities as Record<string, number>).sort((a, b) => b[1] - a[1])
  const [top, p1] = probs[0] ?? ['none', 0]; const p2 = probs[1]?.[1] ?? 0
  const hit = ranked.find(({ c }) => `r${c.n.ref}` === top)
  if (!hit || p1 < bar) return null
  if (p1 - p2 < MARGIN) {
    const near = probs.slice(0, 3).map(([k, p]) => `${k === 'none' ? 'none' : opts[k]} ${p.toFixed(2)}`).join('; ')
    throw new Surprise(`ambiguous target "${target}": ${near}; name the element more exactly`, await seen())
  }
  return { ...hit.c, how: `jev(${p1.toFixed(2)})`, p: p1, margin: p1 - p2 }
}

const press = (combo: string) => desk.key(combo)

// a shown control takes a pointer click, since its AT-SPI action runs the handler inside the a11y bridge's dispatch, where a
// modal dialog it opens blocks the app; a tab, option or radio is selected; a hidden menu entry opens its menus first
async function act(n: Node) {
  const t0 = Date.now()
  try { return (await desk.press(n.id)).how } finally { tick('act', t0) }
}

// a menu entry with a shortcut takes the shortcut, since an AT-SPI action on it runs its handler inside the a11y bridge's
// D-Bus dispatch: a modal dialog it opens answers no AT-SPI call, and the first key deadlocks the app
async function choose(nodes: Node[], n: Node, rec: Record<string, unknown>, open = false) {
  const t0 = Date.now()
  try {
    const mods = n.shortcut.split('+').slice(0, -1)
    if (n.shortcut && (mods.some((x) => x !== 'shift') || /^f\d+$/.test(n.shortcut))) {
      if (open) for (let q = n.parent; q >= 0 && nodes[q].role === 'menu'; q = nodes[q].parent) await press('Escape')
      try { await press(n.shortcut); rec.via = 'shortcut'; return } catch {}
    }
    rec.via = await act(n)
  } finally { tick('act', t0) }
}

async function settle(before: string | undefined) {
  const t0 = Date.now()
  for (let i = 0; i < 6 && (await focused())?.title === before; i++) await sleep(25)
  tick('settle', t0)
}

async function ready(w: Win) {
  const t0 = Date.now()
  for (;;) {
    let nodes: Node[]; try { nodes = parse(await snapOf(w)) } catch { break }
    if (!nodes.filter((n) => n.depth === 1 && n.role === 'button').at(-1)?.disabled || Date.now() - t0 >= 400) break
    await sleep(20)
  }
  tick('ready', t0)
}

// a typed line that ends in a newline submits a file chooser: Return until it closes, since the first one may only close the completion popup
async function submit(w: Win, chooser = false) {
  if (chooser) await ready(w)
  const t0 = Date.now()
  const [title, pid] = [w.title, w.pid]
  const open = () => titled(title, pid)
  for (let i = 0; i < 3 && await open(); i++) {
    await press('Return')
    for (let k = 0; k < 15 && await open(); k++) await sleep(40)
  }
  tick('submit', t0)
  if (await open() && (await focused())?.title === title) throw new Surprise(`"${title}" is still open after Return`, await seen())
}

async function closed(title: string, pid: number | null) {
  const t0 = Date.now()
  const open = () => titled(title, pid)
  const landed = async () => { const f = await focused(); return !!f && f.title !== title && f.pid !== null }
  for (let k = 0; k < 25 && await open(); k++) await sleep(40)
  for (let k = 0; k < 25 && !(await open()) && !(await landed()); k++) await sleep(20)
  tick('settle', t0); return !(await open())
}

// the shown field a plan's first typing filled from empty, so a replay can check it is still empty before it types; window and tab names are no part of it
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
    const d = (await desk.windows()).find((x) => x.title !== w.title && x.pid === w.pid); if (!d) return null
    const ids = parse(await snapOf(d)).filter((n) => n.role === 'textbox').map((n) => n.id)
    return async () => (await Promise.all(ids.map(async (id) => { try { return (await desk.read(id, { text: true })).value ?? '' } catch { return '' } }))).join('\n')
  } catch { return null } finally { tick('verify', t0) }
}
async function replaced(nodes: Node[], read: () => Promise<string>, before: string) {
  const t0 = Date.now(); let now = await read()
  for (let i = 0; i < 10 && now === before; i++) { await sleep(30); now = await read() }
  const f = nodes.find((n) => n.role === 'textbox'); let find = ''
  try { find = f ? (await desk.read(f.id)).value ?? '' : '' } catch {}
  tick('verify', t0)
  if (now === before && find && now.includes(find)) throw new Surprise(`Replace All replaced nothing: "${find}" is still in the document`, await seen())
}

async function setState(s: Step, hit: Hit, rec: Record<string, unknown>) {
  const on = s.on ?? true
  let was = await checked(hit.n)
  if (was === null) was = await cropState(hit.n)
  rec.was = was
  if (was === on) { rec.noop = true; return }
  await act(hit.n)
  let now = await checked(hit.n)
  for (let i = 0; i < 5 && now !== on && now !== null; i++) { await sleep(30); now = await checked(hit.n) }
  if (now === null) now = await cropState(hit.n)
  if (now !== on) throw new Surprise(`"${hit.n.name}" is still ${now ? 'on' : 'off'}`, await seen())
}

async function cropState(n: Node) {
  const b = (await desk.read(n.id)).bounds ?? n.bounds
  if (!b) throw new Error(`"${n.name}" has no screen position`)
  const shot = await desk.shot({ x: Math.max(0, b[0] - 40), y: Math.max(0, b[1] - 4), w: b[2] + 80, h: b[3] + 8 })
  return (await vision(`This is one control of a desktop app: "${n.name}". Is it switched on (a check mark, tick, filled box or pressed state)? {"on": true or false}`, shot)).on === true
}

async function run(s: Step, rec: Record<string, unknown>) {
  if (s.app) {
    const before = new Set((await desk.windows()).map((w) => w.title))
    let launched = false
    const name = norm(s.title ?? s.app); const bare = !s.title && /^[\w.-]+$/.test(s.app)
    const own = (w: Win) => norm(w.title).includes(name) || (bare && (norm(w.title).includes(name.replace(/-/g, ' ')) || ownedBy(w.pid, name)))
    const has = async () => { const ws = await desk.windows(); return ws.find(own) ?? (launched ? ws.find((w) => w.title && !before.has(w.title)) : undefined) }
    let w = await has()
    const listed = !w && bare ? await desk.focus(s.app).catch(() => null) : null
    if (listed) { found = true; known.add(listed.title); rec.window = lastSeen = listed.title; return }
    if (!w) {
      const t0 = Date.now(); launched = true; let missing = false
      try { await desk.launch(s.app) } catch (e) {
        if (!/^[\w.-]+$/.test(s.app)) throw e
        spawn(s.app, [], { detached: true, stdio: 'ignore' }).on('error', () => { missing = true }).unref()
      }
      for (const end = Date.now() + 8000; !(w = await has()); await sleep(50)) {
        if (missing) throw new Surprise(`no app "${s.app}"`, await seen())
        if (Date.now() > end) throw new Surprise(`app "${s.app}" has no window`, await seen())
      }
      tick('launch', t0)
    }
    found = !launched; if (!w.active) await desk.focus(w.title, w.pid); known.add(w.title); rec.window = lastSeen = w.title; await windowFor(w.title); return
  }
  if (s.type !== undefined) {
    const w = await current(); const t0 = Date.now(); const nl = s.type.endsWith('\n')
    const dialog = !s.type.includes('\n') && !!w && (await desk.windows()).some((x) => x.title !== w.title && x.pid === w.pid)
    const o = (nl || dialog) && w ? await filled(w) : ''
    if (blocked(o)) throw new Surprise(unanswered(w!.title), await seen())
    const chooser = isChooser(o); const sub = chooser && nl
    const first = !typed; typed = true
    if (s.blank && found && w && await holdsText(w, s.blank)) throw new Surprise(`"${w.title}" already holds text where this flow typed into an empty field, and typing would add to it: open a new document (for example {key: "ctrl+n"}) or empty the field, then replay the flow`, await seen())
    // GTK preselects the name up to its last dot: a bare name keeps the extension, a path or a name with its own extension replaces the whole field
    if (chooser && /[/\\]|\.[a-z][a-z0-9]{0,7}\n?$/i.test(s.type)) await selectLine()
    // a line break goes as Return, which GTK text views take where they ignore a Linefeed
    for (const [i, part] of (sub ? s.type.slice(0, -1) : s.type).split(/\r?\n/).entries()) { if (i) await press('Return'); if (part) await desk.typeText(part) }
    tick('act', t0)
    if (recording && first && !chooser && w) rec.blank = await typedInto(w, s.type)
    const v = nl && !sub ? await focused() : null
    if (sub) { rec.submit = true; await submit(w!, true) } else if (v && isDialog(v.title === w?.title ? o : await outline(v)) && await closed(v.title, v.pid)) rec.submit = true
    if (dialog && w) {
      const want = s.type.slice(0, 30).replace(/"/g, '\\"'); let after = ''
      for (let k = 0; k < 5; k++) { after = await outline(w); if (!isDialog(after) || after.split('\n').some((l) => /^\s*- (textbox|searchbox|combobox|spinbutton)\b.*: ".*/.test(l) && l.includes(want))) return; await sleep(40) }
      throw new Surprise(`"${s.type}" reached no text field of "${w.title}" (Tab can land on a button): fill the field with {set: label, value}`, await seen())
    }
    return
  }
  if (s.key) {
    const w = await focused()
    const o = /^(enter|return)$/i.test(s.key) && w ? await filled(w) : ''
    if (blocked(o)) throw new Surprise(unanswered(w!.title), await seen())
    if (isChooser(o)) { rec.submit = true; await submit(w!, true); return }
    const t0 = Date.now(); await press(s.key); tick('act', t0)
    if (w && /\b(ctrl|control|alt)\+|^f\d+$/i.test(s.key)) pressed = { title: w.title, at: Date.now() }
    if (isDialog(o) && await closed(w!.title, w!.pid)) rec.submit = true
    return
  }
  if (s.wait) { const w = await windowFor(s.wait, s.timeout_ms ?? 5000, s.seen); rec.seen = lastSeen = w.title; return }
  if (s.close) {
    // close shuts a dialog: the named window, else the focused dialog, else a dialog of the focused app; never an app's main window unless named
    const t0 = Date.now(); let w: Win | null = s.window ? await windowFor(s.window, 3000, s.seen) : await focused()
    const o = w && !s.window ? await outline(w) : ''
    const others = w ? (await desk.windows()).filter((x) => x.pid === w!.pid && x.title !== w!.title) : []
    if (w && blocked(o) && others.length) { rec.seen = w.title; rec.how = 'wm'; await desk.close(w.title, w.pid); await settle(w.title); tick('act', t0); return }
    if (w && !s.window && !isDialog(o)) { let d: Win | null = null; for (const x of others) if (isDialog(await outline(x))) { d = x; break }; w = d }
    if (w && blocked(o)) throw new Surprise(unanswered(w.title), await seen())
    if (!w) { rec.noop = true; tick('act', t0); return }
    const [title, pid] = [w.title, w.pid]; rec.seen = title; if (!w.active) await desk.focus(title, pid); await press('Escape'); await settle(w.title)
    if (await titled(title, pid)) await desk.close(title, pid)
    tick('act', t0); await closed(title, pid); return
  }
  const w = await windowFor(s.window, 3000, s.seen)
  rec.seen = w.title; if (w.title.trim()) lastSeen = w.title
  let t0 = Date.now(); let snap: desk.Snap
  try { snap = await snapOf(w, !!(s.menu || s.click)) } catch (e) {
    if (blocked(String(e))) throw new Surprise(unanswered(w.title), await seen())
    throw e
  }
  let nodes = parse(snap); tick('snap', t0)
  if (s.expect) {
    const d = await decide({ window_title: w.title, contents: render(nodes, 60, false).slice(0, 4000) }, { ok: desk.yes(s.expect) })
    rec.p = +d.ok.probability.toFixed(2)
    if (d.ok.probability >= 0.5) return
    const v = await vision(`You check the state of a computer screen.\nCondition: ${s.expect}\n{"ok": true or false, "why": "one short sentence"}`, await desk.shot()); rec.verdict = v
    if (!v.ok) throw new Surprise(`expectation not met: ${s.expect} (${v.why})`, await seen())
    return
  }
  if (s.verify) {
    const hit = await resolve(nodes, s.verify, s.role ?? (s.value !== undefined ? FIELDS : s.on !== undefined ? `${TOGGLES}|menuitem` : undefined), undefined, 0.7, s.fp)
    if (!hit) { if (s.on === false || s.enabled === false) return; throw new Surprise(`verify: no element matches "${s.verify}"`, await seen()) }
    rec.how = hit.how; rec.fp = fingerprint(nodes, hit)
    const bad: string[] = []
    if (s.value !== undefined) { let v = ''; try { v = (await desk.read(hit.n.id)).value ?? '' } catch (e) { if (blocked(String(e))) throw new Surprise(unanswered(w.title), await seen()); v = hit.n.value }; if (!norm(v).includes(norm(s.value))) bad.push(`reads "${v}"`) }
    if (s.enabled !== undefined && hit.n.disabled === s.enabled) bad.push(hit.n.disabled ? 'is disabled' : 'is enabled')
    if (s.on !== undefined) { const c = (w.pid === null ? null : gtkToggle(w.pid, hit.n.name)) ?? (await checked(hit.n)) ?? await cropState(hit.n); rec.was = c; if (c !== s.on) bad.push(c ? 'is on' : 'is off') }
    if (bad.length) throw new Surprise(`verify: "${hit.n.name || s.verify}" ${bad.join(', ')}`, await seen())
    return
  }
  if (s.menu) {
    let within: number | undefined
    let hit: Hit | null = null
    const parts = s.menu.split('>').map((x) => x.trim())
    const soft = async (target: string, role: string | undefined, inside?: number) => { try { return await resolve(nodes, target, role, inside, 0.7) } catch (e) { if (e instanceof Surprise) return null; throw e } }
    hit = byFp(nodes, s.fp)
    if (!hit) {
      let prev: Hit | null = null
      for (const [k, part] of parts.entries()) {
        // a Windows menu lists its entries only while it is open
        if (OS === 'win32' && prev?.n.role === 'menu' && !nodes.some((x) => x.parent === prev!.i)) {
          await act(prev.n); rec.opened = ((rec.opened as number) ?? 0) + 1; within = undefined
          for (let t = 0; t < 10; t++) { nodes = parse(await snapOf(w, true)); if (candidates(nodes).some(({ n }) => norm(n.name) === norm(part))) break; await sleep(30) }
        }
        hit = await soft(part, k < parts.length - 1 ? 'menu' : undefined, within) ?? await soft(part, undefined, within)
        if (!hit) {
          const leaf = await resolve(nodes, parts.at(-1)!, 'menuitem|menu|menuitemcheckbox|menuitemradio')
          if (leaf?.how !== 'ax') throw new Surprise(`menu entry "${part}" not found`, await seen())
          hit = leaf; rec.how = 'ax-anywhere'; rec.path = [] as string[]
          for (let q = leaf.n.parent; q >= 0 && nodes[q].role === 'menu'; q = nodes[q].parent) (rec.path as string[]).unshift(nodes[q].name)
          break
        }
        within = hit.i; rec.how = hit.how; prev = hit
      }
    } else rec.how = 'fp'
    rec.fp = fingerprint(nodes, hit!)
    if (hit!.p !== undefined) rec.pick = [hit!.p, hit!.margin]
    const pid = w.pid
    if (s.on === undefined) { if (rec.opened) rec.via = await act(hit!.n); else await choose(nodes, hit!.n, rec); await settle(w.title); const now = pid === null ? null : gtkToggle(pid, hit!.n.name); if (now !== null) rec.now = now; return }
    const was = pid === null ? null : gtkToggle(pid, hit!.n.name)
    if (was !== null) {
      rec.was = was; rec.state = 'gtk'
      if (was === s.on) { rec.noop = true; return }
      await act(hit!.n)
      let now = gtkToggle(pid!, hit!.n.name)
      for (let i = 0; i < 5 && now !== s.on; i++) { await sleep(30); now = gtkToggle(pid!, hit!.n.name) }
      if (now !== s.on) throw new Surprise(`menu toggle "${hit!.n.name}" is still ${now ? 'on' : 'off'}`, await seen())
      return
    }
    if (hit!.n.role === 'menuitemcheckbox' || hit!.n.role === 'menuitemradio' || OS === 'darwin') {
      const now = await checked(hit!.n); rec.was = now; rec.state = 'atspi'
      if (now === s.on) { for (let k = 0; k < ((rec.opened as number) ?? 0); k++) await press('Escape'); rec.noop = true; return }
      if (now !== null) { await act(hit!.n); await settle(w.title); return }
    }
    const parent = hit!.n.parent >= 0 && nodes[hit!.n.parent].role === 'menu' ? nodes[hit!.n.parent] : null
    if (!parent) throw new Surprise(`"${hit!.n.name}" is not inside a menu`, await seen())
    const chain: Node[] = []
    for (let q: Node | null = parent; q; q = q.parent >= 0 && nodes[q.parent].role === 'menu' ? nodes[q.parent] : null) chain.unshift(q)
    for (const q of chain) { await act(q); await sleep(120) }
    const on = await cropState(hit!.n); rec.was = on; rec.state = 'vision'
    if (on === s.on) { for (const _ of chain) await press('Escape'); rec.noop = true; return }
    await act(hit!.n); await settle(w.title); return
  }
  const target = s.click ?? s.check ?? s.set!
  const hit = await resolve(nodes, target, s.role ?? (s.check ? TOGGLES : s.set ? FIELDS : undefined), undefined, s.set ? 0.5 : 0.7, s.fp)
  if (!hit) {
    if (!s.click) throw new Surprise(`no element matches "${target}"`, await seen())
    t0 = Date.now()
    const area = await desk.screen()
    const [x, y] = await desk.ground(await desk.shot(), `${target} in the "${w.title}" window`, area)
    tick('ground', t0)
    const under = await desk.at(x, y, w.title).catch(() => ({ found: null, path: [] }))
    const what = under.found ? [under.found, ...(under.path ?? []).reverse()].filter((n) => n.name).map((n) => `${n.role} "${n.name}"`).join(' under ') : ''
    rec.ground_hit = what.slice(0, 80)
    if (!what.trim() || (await decide({ element_under_point: what, window_title: w.title }, { is: desk.yes(`Is this element "${target}"?`) })).is.probability < 0.7)
      throw new Surprise(`no element matches "${target}"${what.trim() ? `; grounding pointed at ${what.slice(0, 80)}` : ''}`, await seen())
    t0 = Date.now(); await desk.click(x, y); tick('act', t0)
    rec.how = 'ground'; rec.unsaved = true; await settle(w.title); return
  }
  rec.how = hit.how; rec.ref = `${hit.n.role} "${hit.n.name}"`; rec.fp = fingerprint(nodes, hit)
  if (hit.p !== undefined) rec.pick = [+hit.p.toFixed(2), +hit.margin!.toFixed(2)]
  if (hit.n.disabled) {
    t0 = Date.now(); let on = false
    for (let i = 0; i < 15 && !on; i++) { await sleep(40); on = !(await desk.read(hit.n.id)).states.includes('disabled') }
    tick('enable', t0)
    if (!on) throw new Surprise(`${rec.ref} is disabled`, await seen())
  }
  if (s.click && hit.how !== 'fp' && norm(hit.n.name) !== norm(target) && TOGGLES.split('|').includes(hit.n.role))
    throw new Surprise(`"${target}" is closest to ${hit.n.role} "${hit.n.name}" (${hit.how}), and a click would flip it; use {check: "${hit.n.name}", on: ...} for it, or name the element you mean exactly`, await seen())
  if (s.click && hit.n.role === 'menuitem') { await choose(nodes, hit.n, rec, true); await settle(w.title); return }
  if (s.click) {
    const read = norm(hit.n.name) === 'replace all' ? await documents(w) : null; const before = await read?.()
    await act(hit.n)
    if (read) await replaced(nodes, read, before!); else await settle(w.title)
    return
  }
  if (s.check) { await setState(s, hit, rec); return }
  const value = s.value!.endsWith('\n') ? s.value!.slice(0, -1) : s.value!
  const same = (v: string | undefined) => v === undefined || norm(v) === norm(value) || norm(v).includes(norm(value))
  const f = hit.n.role === 'combobox' ? nodes.find((x) => x.parent === hit.i && x.role === 'textbox') ?? hit.n : hit.n
  const lines = ((await desk.read(f.id, { text: true })).value ?? '').includes('\n')
  if (hit.how.startsWith('jev') && lines) throw new Surprise(`"${target}" is closest to a multi-line text (${hit.how}), and set would replace all of it: wait for the window with that field, or name it exactly`, await seen())
  t0 = Date.now()
  await desk.act(f.id, 'focus'); if (f.role === 'textbox' && !lines) await selectLine(); else await press(SELECT_ALL); await desk.typeText(value); if (hit.n.role === 'spinbutton' || hit.n.role === 'slider') { await press('tab'); await press('shift+tab') }
  tick('act', t0)
  t0 = Date.now(); let got: string | undefined
  for (let i = 0; i < 10; i++) { try { got = (await desk.read(f.id)).value ?? '' } catch (e) { if (blocked(String(e))) throw new Surprise(unanswered(w.title), await seen()); got = undefined }; if (same(got)) break; await sleep(40) }
  tick('verify', t0); rec.value = got
  if (!same(got)) throw new Surprise(`"${target}" reads "${got}" after set`, await seen())
  if (s.value!.endsWith('\n')) { rec.submit = true; await submit(w, isChooser(render(nodes, 100, false))) }
}

// {{name}} in any string of a step is a param
function fill(s: Step, p: Record<string, string>): Step {
  const out: any = {}
  for (const [k, v] of Object.entries(s)) out[k] = typeof v === 'string' ? v.replace(/\{\{(\w+)(:base)?\}\}/g, (all, n, b) => p[n] === undefined ? all : b ? p[n].split('/').at(-1)! : p[n]) : v
  return out
}

// values a later request would change become params: set values and typed text, unless the plan already templated them
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
      if (x.length > 2 && v.includes(x)) v = v.replace(x, `{{${n}}}`)
      else if (x.includes('/') && x.split('/').at(-1)!.length > 2 && v.includes(x.split('/').at(-1)!)) v = v.replace(x.split('/').at(-1)!, `{{${n}:base}}`)
    }
    t[k] = v
  }
  return { steps: out, params: p }
}

function git(...args: string[]) { try { execFileSync('git', ['-C', FLOWS, ...args], { stdio: 'ignore', timeout: 5000 }) } catch {} }

function repo() {
  mkdirSync(FLOWS, { recursive: true })
  if (!existsSync(`${FLOWS}/.git`)) {
    git('init', '-q'); writeFileSync(`${FLOWS}/.gitignore`, '.runs.jsonl\n.pending.json\n')
    git('config', 'user.name', 'gui_plan'); git('config', 'user.email', 'gui_plan@box')
  }
}

function writeIndex() {
  const rows = readdirSync(FLOWS).filter((x) => x.endsWith('.json') && !x.startsWith('.')).map((x) => { try { return JSON.parse(readFileSync(`${FLOWS}/${x}`, 'utf8')) as Flow } catch { return null } })
    .filter((x): x is Flow => !!x?.name).sort((a, b) => b.saved_at.localeCompare(a.saved_at))
  writeFileSync(INDEX, rows.map((x) => `${x.name}(${Object.keys(x.params).join(', ')})${x.idempotent ? '' : ' once'}: ${summary(x.steps)}`).join('\n') + '\n')
}

function saveFlow(f: Flow, why: string) {
  repo()
  writeFileSync(`${FLOWS}/${f.name}.json`, JSON.stringify(f, null, 1) + '\n')
  writeIndex()
  git('add', '-A'); git('commit', '-q', '-m', `${f.name}: ${why}`)
}

// what a flow does, in its targets: "Edit > Preferences, Use system monospace font off, Size={{size}}"
function summary(steps: Step[]) {
  const out = steps.filter((s) => !s.app && !s.wait && !s.close && !s.verify).map((s) => s.menu ?? (s.check ? `${s.check} ${s.on === false ? 'off' : 'on'}` : s.set ? `${s.set}=${s.value}` : s.type !== undefined ? `type ${s.type.replace(/\n$/, '')}` : s.key ? `key ${s.key}` : s.click))
  const text = out.join(', ')
  return text.length > 100 ? `${text.slice(0, 99)}…` : text
}

const idempotent = (steps: Step[]) => steps.every((s) => s.type === undefined && (!s.key || /^(esc|escape|tab|shift\+tab)$/i.test(s.key)))
const bare = (s: Step, p: Record<string, string>) => JSON.stringify(Object.entries(fill(s, p)).filter(([k]) => k !== 'fp' && k !== 'seen' && k !== 'blank').sort())

// a chain becomes the flow it names, or one named after its app and last targets
function store(chain: Chain) {
  const tpl = templated(chain.steps, chain.params)
  const keys: Record<string, string> = { 'ctrl+n': 'new', 'ctrl+s': 'save', 'ctrl+shift+s': 'save as', 'ctrl+o': 'open' }
  const targets = chain.steps.filter((s) => s.menu || s.check || s.set || s.click || keys[s.key?.toLowerCase() ?? '']).slice(-2).map((s) => s.key ? keys[s.key.toLowerCase()] : (s.menu ?? s.check ?? s.set ?? s.click)!.split('>').at(-1))
  const auto = slug([chain.steps[0].app, ...targets].filter(Boolean).map((x) => x!.split(' ').slice(0, 3).join(' ')).join(' ')) || undefined
  if (!chain.named && auto && chain.name && auto !== chain.name) git('rm', '-q', '--ignore-unmatch', `${chain.name}.json`)
  if (!chain.named) chain.name = auto
  if (chain.name) saveFlow({ name: chain.name, params: tpl.params, steps: tpl.steps, idempotent: idempotent(chain.steps), runs: 1, saved_at: new Date().toISOString(), rewrites: 0, usage: chain.usage ?? {} }, `saved from ${chain.steps.length} passing steps`)
  return chain.name
}

// {save} alone saves the last plan without running it again; after an escalation, its passed steps while their window is still in front
async function promote(name: string, params: Record<string, string> = {}, rerun = false) {
  let c: Chain | null = null
  try { c = JSON.parse(readFileSync(PENDING, 'utf8')) } catch {}
  if (!c || Date.now() - c.at >= CHAIN_AGE || !(c.saved || c.escalated) || c.dirty || !c.steps.some((s) => !s.app) || !slug(name))
    return { ok: false, reason: 'nothing to save: the last gui_plan run of the last 10 minutes must start at an app step, pass, and click no target found by vision' }
  if (c.escalated || rerun) {
    const f = await focused(); const o = f ? await outline(f) : ''
    if (!f || f.title !== c.window || blocked(o) || isDialog(o)) return { ok: false, reason: `the last plan stopped at a surprise and "${f?.title ?? 'no window'}" is in front: continue it with gui_plan from the failing step, and the passing run is saved` }
  }
  const old = c.saved && !c.named ? c.name : undefined
  c.name = slug(name); c.named = true; c.params = { ...c.params, ...params }
  if (old && old !== c.name) git('rm', '-q', '--ignore-unmatch', `${old}.json`)
  store(c); c.saved = true; c.escalated = false
  try { writeFileSync(PENDING, JSON.stringify(c)) } catch {}
  return { ok: true, saved: c.name, params: templated(c.steps, c.params).params, ...(idempotent(c.steps) ? {} : { note: 'a replay of this flow types again' }) }
}

function loadFlow(name: string): Flow {
  const path = `${FLOWS}/${slug(name)}.json`
  if (!existsSync(path)) throw new Surprise(`no saved flow "${name}"; saved: ${existsSync(INDEX) ? readFileSync(INDEX, 'utf8').trim().split('\n').join(', ') : 'none'}`)
  return JSON.parse(readFileSync(path, 'utf8'))
}

async function find(request: string) {
  const rows = existsSync(INDEX) ? readFileSync(INDEX, 'utf8').trim().split('\n').filter(Boolean) : []
  if (!rows.length) return { ok: false, flows: [] }
  const opts: Record<string, string> = { none: 'none of these flows does it' }
  rows.forEach((r, i) => { opts[`f${i}`] = r })
  const d = await decide({ request }, { pick: desk.choice('Which saved GUI flow does this request?', opts) })
  const k = d.pick.choice as string; const p = d.pick.probabilities[k] as number
  const next = Object.values(d.pick.probabilities as Record<string, number>).sort((a, b) => b - a)[1] ?? 0
  if (k === 'none' || p < 0.6 || p - next < MARGIN) return { ok: false, flow: null, p: +p.toFixed(2) }
  return { ok: true, flow: rows[+k.slice(1)].replace(/\(.*$/, ''), p: +p.toFixed(2) }
}

export async function main(p: Params) {
  return { ...(await perform(p)), usage: desk.usage }
}

async function perform(p: Params) {
  const t0 = Date.now()
  if (p.reindex) { repo(); writeIndex(); git('add', '-A'); git('commit', '-q', '-m', 'sync from the flow library'); return { ok: true } }
  if (p.find) return await find(p.find)
  if (p.save && !p.steps?.length && !p.flow) return await promote(p.save)
  let flow: Flow | null = null
  try { if (p.flow) flow = loadFlow(p.flow) } catch (e) { return { ok: false, done: 0, ms: 0, escalate: { reason: (e as Error).message } } }
  const params = { ...(flow?.params ?? {}), ...(p.params ?? {}) }
  const plan: Step[] = (flow ? flow.steps : p.steps ?? []).filter((s) => !Object.keys(s).every((k) => ['screenshot', 'timeout_s', 'max_edge'].includes(k)))
  if (!plan.length) return { ok: false, error: 'pass steps, flow or find' }
  recording = !flow && !!plan[0].app && !p.start
  // a plan sent again only to save it, while the window it left is still in front, would type again: it is saved from the last run instead
  if (p.save && !flow && !p.start && !idempotent(plan)) {
    let c: Chain | null = null
    try { c = JSON.parse(readFileSync(PENDING, 'utf8')) } catch {}
    const was = c?.steps.filter((s) => !s.close) ?? []; const now = plan.filter((s) => !s.close)
    if (c && was.length === now.length && was.every((s, i) => bare(s, c.params) === bare(now[i], params))) {
      const r = await promote(p.save, p.params, true)
      if (r.ok) return { ...r, ran: false, why: 'these steps passed in the last plan, so they were saved without running them again; call without save to run them' }
    }
  }
  const trace: Record<string, unknown>[] = []; const log: Record<string, unknown>[] = []
  for (const w of await desk.windows()) known.add(w.title)
  const startTitles = new Set(known)
  const start = await focused(); const startWindow = start?.title ?? null; const startPid = start ? start.pid : null
  const startAlert = !start?.title.trim() && isAlert(await outline(start))
  const recs: Record<string, unknown>[] = []
  let escalate: Record<string, unknown> | null = null; let done = plan.length
  const bad = plan.findIndex((s) => OPS.filter((k) => k in s).length !== 1)
  if (bad >= 0) return { ok: false, done: 0, ms: 0, trace, escalate: { step: bad, plan_step: plan[bad], reason: `step ${bad} needs exactly one op of ${OPS.join(', ')}; nothing ran` } }
  const unfilled = (s: Step) => Object.values(s).flatMap((v) => typeof v === 'string' ? [...v.matchAll(/\{\{(\w+)(?::base)?\}\}/g)].map((x) => x[1]) : []).find((n) => params[n] === undefined)
  const gap = plan.findIndex((s, i) => i >= (p.start ?? 0) && unfilled(s))
  if (gap >= 0) return { ok: false, done: 0, ms: 0, trace, escalate: { step: gap, plan_step: plan[gap], reason: `no value for {{${unfilled(plan[gap])}}}: pass it in params; nothing ran` } }
  for (let i = p.start ?? 0; i < plan.length; i++) {
    const s = fill(plan[i], params); T = {}; const rec: Record<string, unknown> = { i, op: OPS.find((k) => k in s) }
    const ts = Date.now()
    try {
      const f = await focused()
      const o = f && (!known.has(f.title) || !f.title.trim()) && !(i === (p.start ?? 0) && startAlert) && !(s.window && norm(f.title).includes(norm(s.window))) ? await outline(f) : ''
      if (blocked(o)) throw new Surprise(unanswered(f!.title), await seen())
      if (isAlert(o)) throw new Surprise(`alert "${f!.title}" is in front`, await seen())
      await run(s, rec)
    } catch (e) {
      const sp = e instanceof Surprise ? e : new Surprise(String((e as Error).message ?? e), await seen())
      escalate = { step: i, plan_step: plan[i], reason: sp.reason, ...sp.seen }; done = i
    }
    rec.ms = Date.now() - ts; log.push({ ...rec, ...T }); recs[i] = rec
    trace.push({ i, op: rec.op, how: rec.how, ms: rec.ms, ...(rec.was !== undefined ? { was: rec.was ? 'on' : 'off', now: rec.noop || s.verify ? undefined : s.on === false ? 'off' : 'on' } : {}), ...(rec.now !== undefined ? { now: rec.now ? 'on' : 'off' } : {}), ...(rec.noop ? { noop: true } : {}) })
    if (escalate) break
  }
  const last = plan.at(-1)!
  let opened = false
  if (!escalate) {
    const ts = Date.now(); const was = (await focused())?.title
    if ((last.key || last.type !== undefined || last.click) && !recs.at(-1)?.submit) {
      for (let i = 0; i < 12 && (await focused())?.title === was; i++) await sleep(40)
      await sleep(80)
    }
    const f = await focused(); const o = f ? await outline(f) : ''
    log.push({ i: plan.length, op: 'end', ms: Date.now() - ts, window: f?.title ?? null })
    const left = f && !startTitles.has(f.title) && isDialog(o) && !(last.wait || last.app || (last.window && norm(f.title).includes(norm(last.window))))
    if (f && blocked(o) && recs.at(-1)?.how !== 'wm') { escalate = { step: plan.length, reason: unanswered(f.title), ...(await seen()) }; done = plan.length }
    else if (f && (isAlert(o) || left)) { escalate = { step: plan.length, reason: `the plan ended with "${f.title || o.split('\n')[0]}" still in front`, ...(await seen()) }; done = plan.length }
    opened = !!f && !startTitles.has(f.title) && isDialog(o)
  }
  let saved: string | undefined
  const jevs = recs.filter((r) => r?.pick).length
  const settled = !escalate && !opened
  if (flow) {
    try { writeFileSync(PENDING, 'null') } catch {}
    if (settled && recs.every((r) => !r?.unsaved)) {
      const rewrites = recs.filter((r, i) => r?.fp && JSON.stringify(r.fp) !== JSON.stringify(flow.steps[i].fp))
      flow.runs += 1
      if (rewrites.length) {
        recs.forEach((r, i) => { if (r?.fp) flow.steps[i].fp = r.fp as Fp; if (r?.seen) flow.steps[i].seen = r.seen as string })
        flow.rewrites += 1; flow.saved_at = new Date().toISOString(); saveFlow(flow, `replay re-resolved ${rewrites.length} step(s)`)
      }
      saved = flow.name
    }
  } else {
    // a task done over several calls is one chain: it starts at a call whose first step is an app, and later calls extend it from the window it left
    const ran = plan.slice(p.start ?? 0, done).map((s, k) => { const r = recs[(p.start ?? 0) + k]; const o: Step = { ...s }; if (r?.fp) o.fp = r.fp as Fp; if (r?.seen && !s.app) o.seen = r.seen as string; if (r?.blank) o.blank = r.blank as Fp; return o })
    const dirty = recs.some((r) => r?.unsaved)
    let chain: Chain | null = null; let prev: Chain | null = null
    try { prev = JSON.parse(readFileSync(PENDING, 'utf8')) } catch {}
    if (plan[0].app && !(p.start ?? 0)) chain = { at: Date.now(), window: null, pid: null, params: { ...(p.params ?? {}) }, steps: ran, dirty, usage: plus({}, desk.usage) }
    else if (prev && Date.now() - prev.at < CHAIN_AGE && (prev.window === startWindow || (prev.pid !== null && prev.pid === startPid))) chain = { ...prev, at: Date.now(), params: { ...prev.params, ...(p.params ?? {}) }, steps: [...prev.steps, ...ran], dirty: prev.dirty || dirty, usage: plus(prev.usage, desk.usage) }
    if (chain) {
      if (p.save) { chain.name = slug(p.save); chain.named = true }
      const changes = chain.steps.some((s) => s.check || s.set || s.type !== undefined || (s.menu && s.on !== undefined))
      chain.escalated = !!escalate; chain.saved = false
      if ((settled || (!escalate && changes)) && !chain.dirty && chain.steps.some((s) => !s.app)) { saved = store(chain); chain.saved = !!saved }
      const end = await focused()
      chain.window = end?.title ?? null
      if (end && !SHELL_WINDOWS.test(end.title)) chain.pid = end.pid
      try { mkdirSync(FLOWS, { recursive: true }); writeFileSync(PENDING, JSON.stringify(chain)) } catch {}
    } else if (prev) try { writeFileSync(PENDING, JSON.stringify({ ...prev, saved: false, escalated: false })) } catch {}
  }
  const ms = Date.now() - t0
  try { mkdirSync(FLOWS, { recursive: true }); appendFileSync(LOG, JSON.stringify({ at: new Date().toISOString(), flow: p.flow ?? null, ok: !escalate, ms, jevs, saved: saved ?? null, usage: desk.usage, steps: log }) + '\n') } catch {}
  const out: Record<string, unknown> = { ok: !escalate, done, ms, trace }
  if (saved) out[flow ? 'flow' : 'saved'] = saved
  if (flow?.usage) out.recorded_usage = flow.usage
  if (escalate) out.escalate = escalate
  else out.window = (await focused())?.title ?? null
  return out
}
