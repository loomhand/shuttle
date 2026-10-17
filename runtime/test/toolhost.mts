import { mkdtempSync } from 'node:fs'
import { createServer } from 'node:net'
import type { Server } from 'node:net'
import { tmpdir } from 'node:os'

export type Call = { tool: string; args: Record<string, unknown> }
export type Reply = string | { text: string; error?: boolean; image?: string }

export async function toolhost(answer: (c: Call) => Reply): Promise<Server> {
  const server = createServer((s) => {
    let buf = Buffer.alloc(0)
    s.on('data', (d: Buffer) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      const r = answer(JSON.parse(buf.subarray(4).toString()) as Call)
      const body = Buffer.from(JSON.stringify(typeof r === 'string' ? { text: r, is_error: false } : { text: r.text, is_error: r.error ?? false, image_b64: r.image }))
      const head = Buffer.alloc(4)
      head.writeUInt32BE(body.length)
      s.end(Buffer.concat([head, body]))
    })
  })
  const socket = `${mkdtempSync(`${tmpdir()}/toolhost-`)}/toolhost.sock`
  await new Promise<void>((r) => server.listen(socket, r))
  process.env.TOOLHOST_SOCKET = socket
  return server
}
