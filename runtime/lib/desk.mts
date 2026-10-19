import { createConnection } from 'node:net'
import { setTimeout as sleep } from 'node:timers/promises'

export type Win = { id: string; title: string; role: string; pid: number | null; active: boolean; app?: string; modal?: string; blocks?: string }
export type El = {
  ref: number; id: string; parent: number; depth: number; role: string; name: string; states: string[]
  value?: string; keys?: string; shortcut?: string; mnemonic?: string; native_role?: string; description?: string; bounds?: [number, number, number, number]
}
export type Snap = { window: Win; nodes: El[]; truncated: boolean; ms: number }
export type Question = { type: 'noul'; instructions: string } | { type: 'choice'; instructions: string; criteria: Record<string, string> }
export type Judgment = { probability?: number; choice?: string; confidence?: number; probabilities?: Record<string, number> }
export type Usage = Record<string, { calls: number; input_tokens: number; output_tokens: number; cached_tokens: number }>
export type ImagePart = { type: 'image_url'; image_url: { url: string } }
export type Served = { model: string; image: boolean; build: string | null; id: string | null }
export type Size = { left?: number; top?: number; width: number; height: number }
type Output = { text: string; is_error: boolean; image_b64?: string }
type Metered = { usage: { model: string; input_tokens: number; output_tokens: number; cached_tokens: number } }
type Decided = Metered & { answers: Record<string, Judgment>; served: Omit<Served, 'image'> }

const LOOK_TIMEOUT = 5_000
const POLL_MS = 40

let screenSize: Size | null = null
let waitless = false
export const usage: Usage = {}
/** One entry per decide/look call: the answering model, whether an image went in, and the build and id it named. */
export const served: Served[] = []

export function call<T>(op: string, args: Record<string, unknown> = {}): Promise<T> {
  return request<T>(`desk_${op}`, args)
}

export const windows = (): Promise<Win[]> => call('windows')
/** Waits until the window list differs from `since` or `timeout_ms` of waiting passes, and returns the list; a toolhost without `desk_wait` is polled. */
export async function wait(since: Win[], timeout_ms: number): Promise<{ changed: boolean; windows: Win[] }> {
  if (!waitless) {
    try {
      return await call('wait', { since, timeout_ms: Math.max(0, Math.round(timeout_ms)) })
    } catch (err: unknown) {
      if (!(err instanceof Error && err.message.startsWith('unknown desk tool'))) throw err
      waitless = true
    }
  }
  await sleep(Math.min(Math.max(0, timeout_ms), POLL_MS))
  const ws = await windows()
  return { changed: JSON.stringify(ws) !== JSON.stringify(since), windows: ws }
}
export const front = (ws: Win[]): Win | null => ws.find((w) => w.active && (w.title !== '' || w.role !== 'window')) ?? null
export async function focused(): Promise<Win | null> {
  return front(await windows())
}
export const snapshot = (window?: string, o: { pid?: number | null; id?: string; keys?: boolean; menus?: boolean; bounds?: boolean } = {}): Promise<Snap> =>
  call('snapshot', { window, pid: o.pid ?? undefined, id: o.id, keys: o.keys, menus: o.menus, bounds: o.bounds })
export const outline = async (window?: string): Promise<string> => (await call<{ text: string }>('snapshot', { window, text: true })).text
export const find = (name: string, o: { role?: string; window?: string } = {}): Promise<{ window: Win; found: El[] }> => call('find', { name, ...o })
export const read = (id: string, o: { text?: boolean } = {}): Promise<El> => call('read', { id, text: o.text })
export const press = (id: string): Promise<{ ok: boolean; how: string }> => call('press', { id })
export const act = (id: string, action: 'click' | 'action' | 'select' | 'focus'): Promise<{ ok: boolean }> => call('act', { id, action })
export const at = (x: number, y: number, window?: string, pid?: number | null): Promise<{ found: El | null; path?: { id: string; role: string; name: string }[] }> => call('at', { x, y, window, pid: pid ?? undefined })
export const key = (keys: string): Promise<unknown> => call('key', { keys })
export const typeText = (text: string): Promise<unknown> => call('type', { text })
export const click = (x: number, y: number, o: { button?: string; count?: number } = {}): Promise<unknown> => call('click', { x, y, ...o })
export const focus = (window: string, pid?: number | null): Promise<{ ok: boolean; title: string; pid: number | null }> => call('focus', { window, pid: pid ?? undefined })
export const close = (window: string, pid?: number | null): Promise<unknown> => call('close', { window, pid: pid ?? undefined })
export const launch = (app: string): Promise<{ ok: boolean; launched: string; suppressed?: boolean }> => call('launch', { app })
export async function screen(): Promise<Size> {
  screenSize ??= await call<Size>('screen')
  return screenSize
}

export async function shot(r: { x?: number; y?: number; w?: number; h?: number } = {}): Promise<Buffer> {
  const out = await exchange('desk_shot', r)
  if (out.image_b64 === undefined) throw new Error('desk_shot returned no image')
  return Buffer.from(out.image_b64, 'base64')
}

/** A setting from the environment; an empty value reads as unset. */
export function setting(k: string): string | undefined {
  return process.env[k] || undefined
}

export const yes = (instructions: string): Question => ({ type: 'noul', instructions })
export const choice = (instructions: string, criteria: Record<string, string>): Question => ({ type: 'choice', instructions, criteria })

/** The decide model over text or JSON state: a yes question answers {probability}, a choice {choice, confidence, probabilities}. */
export const decide = (state: unknown, questions: Record<string, Question>): Promise<Record<string, Judgment>> => judge(state, questions, false)

/** The same over a state that holds PNG parts (image), within 5 s. */
export const look = (state: unknown[], questions: Record<string, Question>): Promise<Record<string, Judgment>> => judge(state, questions, true, LOOK_TIMEOUT)

export function image(png: Buffer): ImagePart {
  return { type: 'image_url', image_url: { url: `data:image/png;base64,${png.toString('base64')}` } }
}

export async function ask(prompt: string, images: Buffer | Buffer[] = []): Promise<string> {
  const pngs = Buffer.isBuffer(images) ? [images] : images
  return (await model<Metered & { text: string }>('model_ask', { prompt, images: pngs.map((p) => p.toString('base64')) })).text
}

/** The point, in desk coordinates, of a described element in a PNG of the desk rectangle `area`. */
export async function ground(png: Buffer, target: string, area: Size): Promise<[number, number]> {
  const r = await model<Metered & { x: number; y: number }>('model_ground', { png_b64: png.toString('base64'), target, area })
  return [r.x, r.y]
}

async function request<T>(tool: string, args: Record<string, unknown>): Promise<T> {
  const out = await exchange(tool, args)
  try { return JSON.parse(out.text) as T } catch { return out.text as T }
}

function exchange(tool: string, args: Record<string, unknown>): Promise<Output> {
  return new Promise((resolve, reject) => {
    const socket = process.env.TOOLHOST_SOCKET
    if (!socket) return reject(new Error('TOOLHOST_SOCKET names no toolhost socket'))
    const s = createConnection(socket)
    const body = Buffer.from(JSON.stringify({ agent_id: process.env.SHUTTLE_AGENT ?? 'desk', tool, args }))
    const head = Buffer.alloc(4)
    head.writeUInt32BE(body.length)
    const chunks: Buffer[] = []
    let got = 0; let want = -1
    s.on('connect', () => { s.write(head); s.write(body) })
    s.on('data', (d: Buffer) => {
      chunks.push(d); got += d.length
      if (want < 0 && got >= 4) want = 4 + (chunks.length === 1 ? d : Buffer.concat(chunks)).readUInt32BE(0)
      if (want < 0 || got < want) return
      s.destroy()
      const out = JSON.parse(Buffer.concat(chunks, got).subarray(4, want).toString()) as Output
      if (out.is_error) return reject(new Error(out.text))
      resolve(out)
    })
    s.on('end', () => reject(new Error(`${tool}: the toolhost closed the connection mid-frame`)))
    s.on('error', reject)
  })
}

async function model<T extends Metered>(tool: string, args: Record<string, unknown>): Promise<T> {
  const r = await request<T>(tool, args)
  const u = usage[r.usage.model] ??= { calls: 0, input_tokens: 0, output_tokens: 0, cached_tokens: 0 }
  u.calls += 1
  u.input_tokens += r.usage.input_tokens
  u.output_tokens += r.usage.output_tokens
  u.cached_tokens += r.usage.cached_tokens
  return r
}

async function judge(state: unknown, questions: Record<string, Question>, image: boolean, timeout_ms?: number): Promise<Record<string, Judgment>> {
  const rec: Served = { model: image ? 'verify' : 'decide', image, build: null, id: null }
  served.push(rec)
  const r = await model<Decided>('model_decide', { state, questions, image, timeout_ms })
  Object.assign(rec, r.served)
  return r.answers
}
