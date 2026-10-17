import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { test } from 'node:test'

type Body = { messages: { content: { type: string; image_url?: { url: string } }[] }[] }

test('ask takes one PNG buffer or a list of them', async () => {
  const bodies: Body[] = []
  const gw = createServer((req, res) => {
    const chunks: Buffer[] = []
    req.on('data', (c: Buffer) => chunks.push(c))
    req.on('end', () => {
      bodies.push(JSON.parse(Buffer.concat(chunks).toString()) as Body)
      res.setHeader('content-type', 'application/json')
      res.end(JSON.stringify({ choices: [{ message: { content: 'ok' } }] }))
    })
  })
  await new Promise<void>((r) => gw.listen(0, '127.0.0.1', r))
  for (const k of ['http_proxy', 'HTTP_PROXY']) delete process.env[k]
  const addr = gw.address()
  assert.ok(addr !== null && typeof addr === 'object')
  process.env.GW_BASE_URL = `http://127.0.0.1:${addr.port}`
  const desk = await import('../lib/desk.mts')
  const png = Buffer.from('png')
  try {
    assert.equal(await desk.ask('one', png), 'ok')
    assert.equal(await desk.ask('two', [png, png]), 'ok')
    assert.equal(await desk.ask('none'), 'ok')
  } finally {
    gw.close()
  }
  const images = bodies.map((b) => b.messages[0]?.content.filter((c) => c.type === 'image_url').map((c) => c.image_url?.url))
  const url = `data:image/png;base64,${png.toString('base64')}`
  assert.deepEqual(images, [[url], [url, url], []])
})
