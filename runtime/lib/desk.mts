// desk.mts: the desk API for scripts and gui_plan: the desktop through the toolhost socket, models through gw over HTTP
import { readFileSync } from 'node:fs'
import { request } from 'node:http'
import { createConnection } from 'node:net'
import { homedir } from 'node:os'

export type Win = { id: string; title: string; role: string; pid: number | null; active: boolean; modal?: string; blocks?: string }
export type El = {
  ref: number; id: string; parent: number; depth: number; role: string; name: string; states: string[]
  value?: string; keys?: string; shortcut?: string; mnemonic?: string; native_role?: string; description?: string; bounds?: [number, number, number, number]
}
export type Snap = { window: Win; nodes: El[]; truncated: boolean; ms: number }
export type Question = { type: 'noul'; instructions: string } | { type: 'choice'; instructions: string; criteria: Record<string, string> }
export type Usage = Record<string, { calls: number; input_tokens: number; output_tokens: number; cached_tokens: number }>

const SOCKETS: Record<string, string> = { win32: '\\\\.\\pipe\\cocoonbox-desk', darwin: `${homedir()}/Library/Caches/cocoonbox/desk.sock` }
const SOCKET = process.env.TOOLHOST_SOCKET || SOCKETS[process.platform] || '/run/cocoonbox/toolhost.sock'
const ENV_FILE = '/etc/cocoonbox/desk.env'
const DECIDE_MODEL = 'typesafe/jev-1.13'
const LOOK_MODEL = 'typesafe/jev-1.14'
const LOOK_BUILD = 'typesafe/jev-1.14-20261014'
const LOOK_TIMEOUT = 5_000
const ASK_MODEL = 'anthropic/claude-sonnet-5'
const GROUND_MODEL = 'qwen/qwen3-vl-235b-a22b-instruct'
const MODEL_TIMEOUT = 60_000
let screenSize: { width: number; height: number } | null = null
let gwConfig: { base: string; key: string } | null = null
export const usage: Usage = {}

// one toolhost call: a big-endian u32 length and JSON each way
export function call(op: string, args: Record<string, unknown> = {}): Promise<any> {
  return new Promise((resolve, reject) => {
    const s = createConnection(SOCKET)
    const body = Buffer.from(JSON.stringify({ agent_id: 'desk', tool: `desk_${op}`, args }))
    const head = Buffer.alloc(4)
    head.writeUInt32BE(body.length)
    let buf = Buffer.alloc(0)
    s.on('connect', () => s.write(Buffer.concat([head, body])))
    s.on('data', (d) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      s.destroy()
      const out = JSON.parse(buf.subarray(4, 4 + buf.readUInt32BE(0)).toString())
      if (out.is_error) return reject(new Error(out.text))
      try { resolve(JSON.parse(out.text)) } catch { resolve(out.text) }
    })
    s.on('error', reject)
  })
}

export const windows = (): Promise<Win[]> => call('windows')
export const front = (ws: Win[]): Win | null => ws.find((w) => w.active && (w.title !== '' || w.role !== 'window')) ?? null
export async function focused(): Promise<Win | null> {
  return front(await windows())
}
export const snapshot = (window?: string, o: { pid?: number | null; keys?: boolean; bounds?: boolean } = {}): Promise<Snap> =>
  call('snapshot', { window, pid: o.pid ?? undefined, keys: o.keys, bounds: o.bounds })
export const outline = async (window?: string): Promise<string> => (await call('snapshot', { window, text: true })).text
export const find = (name: string, o: { role?: string; window?: string } = {}): Promise<{ window: Win; found: El[] }> => call('find', { name, ...o })
export const read = (id: string, o: { text?: boolean } = {}): Promise<El> => call('read', { id, text: o.text })
export const press = (id: string): Promise<{ ok: boolean; how: string }> => call('press', { id })
export const act = (id: string, action: 'click' | 'action' | 'select' | 'focus'): Promise<{ ok: boolean }> => call('act', { id, action })
export const at = (x: number, y: number, window?: string): Promise<{ found: El | null; path?: { id: string; role: string; name: string }[] }> => call('at', { x, y, window })
export const key = (keys: string) => call('key', { keys })
export const typeText = (text: string) => call('type', { text })
export const click = (x: number, y: number, o: { button?: string; count?: number } = {}) => call('click', { x, y, ...o })
export const focus = (window: string, pid?: number | null) => call('focus', { window, pid: pid ?? undefined })
export const close = (window: string, pid?: number | null) => call('close', { window, pid: pid ?? undefined })
export const launch = (app: string) => call('launch', { app })
export async function screen(): Promise<{ width: number; height: number }> {
  screenSize ??= await call('screen')
  return screenSize!
}
// a PNG of a screen rectangle, the whole screen by default
export async function shot(r: { x?: number; y?: number; w?: number; h?: number } = {}): Promise<Buffer> {
  return Buffer.from((await call('shot', r)).png_b64, 'base64')
}

function gw() {
  if (gwConfig) return gwConfig
  let file = ''
  try { file = readFileSync(ENV_FILE, 'utf8') } catch {}
  const get = (k: string) => process.env[k] || [...file.matchAll(/^(\w+)=(.*)$/gm)].find((m) => m[1] === k)?.[2]?.trim()
  const base = get('GW_BASE_URL')
  if (!base) throw new Error(`no GW_BASE_URL in the environment or ${ENV_FILE}`)
  gwConfig = { base: base.replace(/\/+$/, ''), key: get('GW_API_KEY') || 'unprovisioned' }
  return gwConfig
}

// a plain HTTP proxy request (absolute URL, no CONNECT), so the box's egress proxy can set gw's key
function post(path: string, body: { model: string } & Record<string, unknown>, timeout = MODEL_TIMEOUT): Promise<any> {
  const { base, key } = gw()
  const url = new URL(`${base}/${path}`)
  const proxy = process.env.http_proxy || process.env.HTTP_PROXY
  const bypass = (process.env.no_proxy || process.env.NO_PROXY || '').split(',').map((h) => h.trim()).filter(Boolean)
  const via = proxy && url.protocol === 'http:' && !bypass.includes(url.hostname) ? new URL(proxy) : null
  const data = Buffer.from(JSON.stringify(body))
  const headers = { 'content-type': 'application/json', 'content-length': data.length, authorization: `Bearer ${key}`, host: url.host }
  const target = via ? { host: via.hostname, port: via.port || 80, path: url.href } : { host: url.hostname, port: url.port || 80, path: url.pathname + url.search }
  return new Promise((resolve, reject) => {
    const req = request({ ...target, method: 'POST', headers, timeout }, (res) => {
      const chunks: Buffer[] = []
      res.on('data', (c) => chunks.push(c))
      res.on('end', () => {
        const text = Buffer.concat(chunks).toString()
        if ((res.statusCode ?? 500) >= 300) return reject(new Error(`gw ${path} returned ${res.statusCode}: ${text.slice(0, 300)}`))
        try {
          const r = JSON.parse(text)
          const u = usage[body.model] ??= { calls: 0, input_tokens: 0, output_tokens: 0, cached_tokens: 0 }
          u.calls += 1
          u.input_tokens += r.usage?.prompt_tokens ?? r.usage?.input_tokens ?? 0
          u.output_tokens += r.usage?.completion_tokens ?? r.usage?.output_tokens ?? 0
          u.cached_tokens += r.usage?.prompt_tokens_details?.cached_tokens ?? 0
          resolve(r)
        } catch (e) { reject(e) }
      })
    })
    req.on('timeout', () => req.destroy(new Error(`gw ${path} timed out`)))
    req.on('error', reject)
    req.end(data)
  })
}

export const yes = (instructions: string): Question => ({ type: 'noul', instructions })
export const choice = (instructions: string, criteria: Record<string, string>): Question => ({ type: 'choice', instructions, criteria })

// gw's decisions model over text or JSON state: a yes question answers {probability}, a choice {choice, confidence, probabilities}
export async function decide(state: unknown, questions: Record<string, Question>): Promise<Record<string, any>> {
  return answers(await post('decisions', { model: DECIDE_MODEL, state, questions }))
}

// the same over a state that holds PNG parts (image), answered by one pinned build of jev-1.14: another build, an error or 5 s throws
export async function look(state: unknown[], questions: Record<string, Question>): Promise<Record<string, any>> {
  const r = await post('decisions', { model: LOOK_MODEL, state, questions }, LOOK_TIMEOUT)
  if (r.model !== LOOK_BUILD) throw new Error(`gw decisions served ${r.model} for ${LOOK_MODEL}`)
  return answers(r)
}

function answers(r: any) {
  const out: Record<string, any> = {}
  for (const [k, a] of Object.entries(r.answers ?? {}) as [string, any][]) out[k] = a.type === 'noul' ? { probability: a.noul } : a
  return out
}

export function image(png: Buffer) {
  return { type: 'image_url', image_url: { url: `data:image/png;base64,${png.toString('base64')}` } }
}

// a vision model's answer about PNG images
export async function ask(prompt: string, images: Buffer | Buffer[] = []): Promise<string> {
  const pngs = Buffer.isBuffer(images) ? [images] : images
  const r = await post('chat/completions', { model: ASK_MODEL, max_tokens: 400, messages: [{ role: 'user', content: [{ type: 'text', text: prompt }, ...pngs.map(image)] }] })
  return r.choices?.[0]?.message?.content ?? ''
}

// the screen point of a described element in a PNG of the screen at (left, top)
export async function ground(png: Buffer, target: string, area: { left?: number; top?: number; width: number; height: number }): Promise<[number, number]> {
  const prompt = `Locate the ${target} in this screenshot. Answer with one JSON object and nothing else: {"point_2d": [x, y]}, the centre of the element in coordinates from 0 to 1000 across and down the image.`
  const r = await post('chat/completions', { model: GROUND_MODEL, max_tokens: 100, messages: [{ role: 'user', content: [image(png), { type: 'text', text: prompt }] }] })
  const text: string = r.choices?.[0]?.message?.content ?? ''
  const g = /(-?\d+(?:\.\d+)?)\s*,\s*(-?\d+(?:\.\d+)?)/.exec(text)
  if (!g) throw new Error(`grounding found no point for ${target}: ${text.slice(0, 120)}`)
  return [Math.round((area.left ?? 0) + (+g[1] / 1000) * area.width), Math.round((area.top ?? 0) + (+g[2] / 1000) * area.height)]
}
