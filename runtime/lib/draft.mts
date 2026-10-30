import * as desk from './desk.mts'
import { ring, unpng } from './png.mts'

type Hit = { role?: string; name?: string; description?: string }
type Field = { role: string; name: string; frame: string; secure?: boolean }
type Event = {
  t: number; kind: 'click' | 'key' | 'after' | 'note' | 'skipped' | 'password'
  x?: number; y?: number; button?: string; count?: number; mods?: string[]; app?: string | null
  element?: Hit | null; path?: { role: string; name: string }[]; root?: boolean
  window?: { title?: string; bounds?: [number, number, number, number] | null } | string
  crop?: string; crop_scale?: number; crop_offset?: [number, number]; focus?: Field | null; code?: number; text?: string; secret?: boolean; role?: string
}
type Recording = { id: string; apps?: string[]; events: Event[] }
type Step = Record<string, unknown> & { intent: string }
type Params = { recording: Recording; params?: Record<string, string> }
type Draft = { id: string; apps: string[]; params: Record<string, { example?: string; secret?: true }>; steps: Step[]; notes: { step: number; text: string }[]; gaps: number[] }

const KEYS: Record<number, string> = { 36: 'return', 76: 'enter', 48: 'tab', 53: 'escape', 51: 'backspace', 117: 'delete', 123: 'left', 124: 'right', 125: 'down', 126: 'up', 115: 'home', 119: 'end', 116: 'pageup', 121: 'pagedown' }
const NAMED_ROLES = new Set(['button', 'checkbox', 'radio', 'tab', 'link', 'switch', 'togglebutton', 'menuitem'])
const CAPTION = 'The image is a crop of a screen; a red ring with four ticks marks the point a user clicked. Name the control under the ring as a short noun phrase a person would use to find it on screen, such as: the Save button. At most 8 words; answer with the phrase only.'
const RING_PT = 14

/** Turns a recording into a draft gui_plan: one step per action with its intent, the recorded crop and point of each click, the window each action left in front, and `params` for the typed values the caller names. */
export async function main(p: Params): Promise<Draft> {
  const examples = Object.entries(p.params ?? {}).filter(([, v]) => v).sort((a, b) => b[1].length - a[1].length)
  const params: Draft['params'] = Object.fromEntries(examples.map(([k, v]) => [k, { example: v }]))
  const templ = (s: string) => examples.reduce((out, [k, v]) => out.split(v).join(`{{${k}}}`), s)
  const draft: Draft = { id: p.recording.id, apps: p.recording.apps ?? [], params, steps: [], notes: [], gaps: [] }
  const events = p.recording.events
  const seen = new Set<string>()
  let typed = ''; let secrets = 0; let secret = false; let filled = false
  let into: Field | null | undefined
  let clicked: { e: Event; step: Step } | undefined
  // a run whose recording read no focus checks none on replay; one it never read keeps the plan's own rules
  const focus = (f: Field | null | undefined) => f === null ? { focus: 'unverified' } : f ? { focus: f } : {}
  const password = (f?: Field | null) => {
    const name = `password${secrets ? `_${secrets + 1}` : ''}`; secrets += 1
    params[name] = { secret: true }
    draft.steps.push({ type: `{{${name}}}`, focus: { ...f, secure: true }, intent: `type the ${name.replace('_', ' ')} (a password field: not recorded, pass it as ${name})` })
  }
  const flush = () => {
    if (secret) {
      if (!filled) password(into)
    } else if (typed) {
      const text = templ(typed)
      draft.steps.push({ type: text, ...focus(into), intent: text === typed ? `type ${JSON.stringify(typed)}` : `type ${text}` })
    }
    typed = ''; secret = false; into = undefined
  }
  const first = events.find((e) => e.kind === 'click' || e.kind === 'key')
  const dock = first?.kind === 'click' && !first.app && first.element?.name ? first.element.name : undefined
  const app = dock ?? events.find((e) => (e.kind === 'click' || e.kind === 'key') && e.app)?.app
  const opened = first?.kind === 'click' && !dock && typeof first.window === 'object' ? first.window.title : undefined
  if (app) draft.steps.push({ app, ...(opened ? { title: templ(opened) } : {}), intent: `open ${app}${opened ? ` at "${opened}"` : ''}` })
  for (const [i, e] of events.entries()) {
    if (e.kind === 'key') {
      const mods = e.mods ?? []
      if (e.secret) { if (typed) flush(); if (!secret && e.focus) into = e.focus; secret = true; continue }
      const named = KEYS[e.code ?? -1]
      if (mods.every((m) => m === 'shift') && !named && e.text) { if (secret) flush(); filled = false; if (!typed && 'focus' in e) into = e.focus; typed += e.text; continue }
      flush(); filled = false
      const key = [...mods, named ?? (e.text ?? '').toLowerCase()].join('+')
      draft.steps.push({ key, intent: `press ${key}` })
      continue
    }
    if (e.kind === 'note') { draft.notes.push({ step: draft.steps.length, text: e.text ?? '' }); continue }
    flush()
    if (e.kind === 'password') {
      const last = draft.steps.at(-1)
      if (!filled) password(e.focus)
      else if (last && e.focus && !(last.focus as Partial<Field> | undefined)?.role) last.focus = { ...e.focus, secure: true }
      filled = true
    }
    else if (e.kind === 'skipped') draft.gaps.push(draft.steps.length)
    else if (e.kind === 'after') {
      const w = typeof e.window === 'string' ? e.window : undefined
      const last = draft.steps.at(-1)
      if (w && last && !last.app && !seen.has(w) && e.role === 'window') last.after = templ(w)
      if (w) seen.add(w)
    } else if (e.kind === 'click') {
      if (i === events.indexOf(first as Event) && dock) continue
      const el = e.element ?? {}
      const next = events.slice(i + 1).find((n) => n.kind === 'click')
      if (el.role === 'menu' && next?.element?.role === 'menuitem') continue
      const prev = events.slice(0, i).findLast((n) => n.kind === 'click')
      if (el.role === 'menuitem' && prev?.element?.role === 'menu') {
        const menu = `${prev.element.name} > ${el.name}`
        draft.steps.push({ menu, intent: `choose ${menu}` })
        continue
      }
      const again = (e.count ?? 1) > 1 && clicked && draft.steps.at(-1) === clicked.step && Math.hypot((e.x ?? 0) - (clicked.e.x ?? 0), (e.y ?? 0) - (clicked.e.y ?? 0)) <= 4
      if (again) draft.steps.pop()
      clicked = { e, step: await click(e, templ) }
      draft.steps.push(clicked.step)
      filled = !!e.secret
      if (filled) password()
    }
  }
  flush()
  return draft
}

async function click(e: Event, templ: (s: string) => string): Promise<Step> {
  const el = e.element ?? {}
  const own = !e.root && el.name ? el.name : ''
  const said = own || (e.crop ? await caption(e.crop, e.crop_scale ?? 1, e.crop_offset ?? [0, 0], el.role) : '') || `the ${el.role || 'point'} at ${Math.round(e.x ?? 0)}, ${Math.round(e.y ?? 0)}`
  const target = templ(said)
  const win = typeof e.window === 'object' ? e.window : undefined
  const b = win?.bounds
  const shifted = e.crop_offset?.some((v) => v !== 0) ? { offset: e.crop_offset } : {}
  const recorded: Record<string, unknown> = { text: said, ...(e.crop ? { crop: e.crop, scale: e.crop_scale ?? 1, ...shifted } : {}), ...(e.root ? { root: true } : {}) }
  if (b && e.x !== undefined && e.y !== undefined) Object.assign(recorded, { at: [Math.round(e.x - b[0]), Math.round(e.y - b[1])], size: [b[2], b[3]] })
  return {
    click: target,
    ...(e.button && e.button !== 'left' ? { button: e.button } : {}),
    ...((e.count ?? 1) > 1 ? { count: e.count } : {}),
    ...(e.mods?.length ? { mods: e.mods } : {}),
    ...(own && el.role && NAMED_ROLES.has(el.role) ? { role: el.role } : {}),
    recorded,
    intent: `click ${own ? `the ${el.role} "${own}"` : said}${win?.title ? ` in "${win.title}"` : ''}`,
  }
}

async function caption(crop: string, scale: number, [dx, dy]: [number, number], role?: string): Promise<string> {
  try {
    const img = unpng(Buffer.from(crop, 'base64'))
    const marked = ring(img, Math.round(img.w / 2 + dx * scale), Math.round(img.h / 2 + dy * scale), Math.round(RING_PT * scale))
    const kind = role && role !== 'generic' && role !== 'document' ? ` The accessibility tree calls it a ${role} with no name.` : ''
    return (await desk.ask(`${CAPTION}${kind}`, marked)).trim().replace(/[."]+$/g, '').replace(/^"/, '')
  } catch { return '' }
}
