import { mkdtempSync } from 'node:fs'
import { createServer, type Server } from 'node:net'
import { tmpdir } from 'node:os'
import { setTimeout as sleep } from 'node:timers/promises'
import { crc32, deflateSync } from 'node:zlib'

export const BUILD = 'typesafe/jev-1.14-20261014'
const FLOWS = `${mkdtempSync(`${tmpdir()}/flows-`)}/flows`

process.env.SHUTTLE_FLOWS = FLOWS

export const endpoint = (dir: string): string => process.platform === 'win32' ? `\\\\.\\pipe\\${dir.replace(/[\\/:]/g, '-')}` : `${dir}/toolhost.sock`

export type Call = { tool: string; args: Record<string, unknown> }
export type Reply = string | { text: string; error?: boolean; image?: string }

export async function toolhost(answer: (c: Call) => Reply, o: { wait?: boolean } = {}): Promise<Server> {
  const server = createServer((s) => {
    let buf = Buffer.alloc(0)
    s.on('data', async (d: Buffer) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      const c = JSON.parse(buf.subarray(4).toString()) as Call
      const r = c.tool === 'desk_wait' && o.wait !== false ? await wait(answer, c.args) : answer(c)
      const body = Buffer.from(JSON.stringify(typeof r === 'string' ? { text: r, is_error: false } : { text: r.text, is_error: r.error ?? false, image_b64: r.image }))
      const head = Buffer.alloc(4)
      head.writeUInt32BE(body.length)
      s.end(Buffer.concat([head, body]))
    })
  })
  const dir = mkdtempSync(`${tmpdir()}/toolhost-`)
  await new Promise<void>((r) => server.listen(endpoint(dir), r))
  Object.assign(process.env, { TOOLHOST_SOCKET: endpoint(dir), HOME: dir, USERPROFILE: dir })
  return server
}

export function gray(w: number, h: number, tone = 128): Buffer {
  const chunk = (kind: string, data: Buffer) => {
    const head = Buffer.alloc(8), crc = Buffer.alloc(4); head.writeUInt32BE(data.length); head.write(kind, 4, 'ascii')
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data]))); return Buffer.concat([head, data, crc])
  }
  const ihdr = Buffer.alloc(13); ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = 6
  const raw = Buffer.alloc(h * (w * 4 + 1), tone); for (let y = 0; y < h; y++) raw[y * (w * 4 + 1)] = 0
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))])
}

async function wait(answer: (c: Call) => Reply, args: Record<string, unknown>): Promise<Reply> {
  await sleep(Math.min(Number(args.timeout_ms), 10))
  const r = answer({ tool: 'desk_windows', args: { waiting: true } })
  const text = typeof r === 'string' ? r : r.text
  return JSON.stringify({ changed: text !== JSON.stringify(args.since), windows: JSON.parse(text) })
}
