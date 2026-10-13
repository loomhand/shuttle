import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import type { AddressInfo } from 'node:net'
import { test } from 'node:test'

test('ask takes one PNG buffer or a list of them', async () => {
  const bodies: any[] = []
  const gw = createServer((req, res) => {
    const chunks: Buffer[] = []
    req.on('data', (c) => chunks.push(c))
    req.on('end', () => {
      bodies.push(JSON.parse(Buffer.concat(chunks).toString()))
      res.setHeader('content-type', 'application/json')
      res.end(JSON.stringify({ choices: [{ message: { content: 'ok' } }] }))
    })
  })
  await new Promise<void>((r) => gw.listen(0, '127.0.0.1', r))
  for (const k of ['http_proxy', 'HTTP_PROXY']) delete process.env[k]
  process.env.GW_BASE_URL = `http://127.0.0.1:${(gw.address() as AddressInfo).port}`
  const desk = await import('../rootfs/opt/desk/lib/desk.mts')
  const png = Buffer.from('png')
  try {
    assert.equal(await desk.ask('one', png), 'ok')
    assert.equal(await desk.ask('two', [png, png]), 'ok')
    assert.equal(await desk.ask('none'), 'ok')
  } finally {
    gw.close()
  }
  const images = bodies.map((b) => b.messages[0].content.filter((c: any) => c.type === 'image_url').map((c: any) => c.image_url.url))
  assert.deepEqual(images, [[`data:image/png;base64,${png.toString('base64')}`], Array(2).fill(`data:image/png;base64,${png.toString('base64')}`), []])
})
