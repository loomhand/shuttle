import { mkdtempSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { setTimeout as sleep } from 'node:timers/promises'

import { png } from '../lib/png.mts'

export const BUILD = 'typesafe/jev-1.14-20261014'
const FLOWS = `${mkdtempSync(`${tmpdir()}/flows-`)}/flows`

process.env.SHUTTLE_FLOWS = FLOWS

export const endpoint = (dir: string): string => process.platform === 'win32' ? `\\\\.\\pipe\\${dir.replace(/[\\/:]/g, '-')}` : `${dir}/daemon.sock`

export type Call = { tool: string; args: Record<string, unknown> }
export type Reply = string | { text: string; error?: boolean; image?: string }

export async function daemon(t: { after(fn: () => void): void }, answer: (c: Call) => Reply): Promise<void> {
  const server = createServer((s) => {
    let buf = Buffer.alloc(0)
    s.on('data', async (d: Buffer) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      const c = JSON.parse(buf.subarray(4).toString()) as Call
      const r = c.tool === 'desk_wait' ? await wait(answer, c.args) : answer(c)
      const body = Buffer.from(JSON.stringify(typeof r === 'string' ? { text: r, is_error: false } : { text: r.text, is_error: r.error ?? false, image_b64: r.image }))
      const head = Buffer.alloc(4)
      head.writeUInt32BE(body.length)
      s.end(Buffer.concat([head, body]))
    })
  })
  const dir = mkdtempSync(`${tmpdir()}/daemon-`)
  await new Promise<void>((r) => server.listen(endpoint(dir), r))
  t.after(() => server.close())
  Object.assign(process.env, { SHUTTLE_SOCKET: endpoint(dir), HOME: dir, USERPROFILE: dir })
}

export function gray(w: number, h: number, tone = 128): Buffer {
  return png(w, h, 6, 4, Buffer.alloc(w * h * 4, tone))
}

async function wait(answer: (c: Call) => Reply, args: Record<string, unknown>): Promise<Reply> {
  await sleep(Math.min(Number(args.timeout_ms), 10))
  const r = answer({ tool: 'desk_windows', args: { waiting: true } })
  const text = typeof r === 'string' ? r : r.text
  return JSON.stringify({ changed: text !== JSON.stringify(args.since), windows: JSON.parse(text) })
}
