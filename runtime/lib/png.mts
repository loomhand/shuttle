import { crc32, deflateSync, inflateSync } from 'node:zlib'

export type Image = { w: number; h: number; type: number; bpp: number; px: Buffer }

// 8-bit RGB or RGBA PNGs, as the daemon writes them
export function unpng(buf: Buffer): Image {
  const idat: Buffer[] = []; let w = 0, h = 0, type = 0
  for (let p = 8; p < buf.length; p += 12 + buf.readUInt32BE(p)) {
    const kind = buf.toString('ascii', p + 4, p + 8), data = buf.subarray(p + 8, p + 8 + buf.readUInt32BE(p))
    if (kind === 'IHDR') { w = data.readUInt32BE(0); h = data.readUInt32BE(4); type = data.readUInt8(9); if (data.readUInt8(8) !== 8 || data.readUInt8(12) !== 0 || (type !== 2 && type !== 6)) throw new Error(`unsupported PNG (depth ${data.readUInt8(8)}, type ${type})`) }
    if (kind === 'IDAT') idat.push(data)
  }
  const bpp = type === 6 ? 4 : 3, stride = w * bpp, raw = inflateSync(Buffer.concat(idat)), px = Buffer.alloc(h * stride)
  if (raw.length !== h * (stride + 1)) throw new Error(`PNG data holds ${raw.length} bytes, not ${h * (stride + 1)}`)
  for (let y = 0; y < h; y++) {
    const f = raw[y * (stride + 1)], row = y * (stride + 1) + 1
    for (let i = 0; i < stride; i++) {
      const a = i >= bpp ? px[y * stride + i - bpp] as number : 0, b = y ? px[(y - 1) * stride + i] as number : 0, c = y && i >= bpp ? px[(y - 1) * stride + i - bpp] as number : 0
      const pa = Math.abs(b - c), pb = Math.abs(a - c), pc = Math.abs(a + b - 2 * c)
      px[y * stride + i] = (raw[row + i] as number) + (f === 1 ? a : f === 2 ? b : f === 3 ? (a + b) >> 1 : f === 4 ? (pa <= pb && pa <= pc ? a : pb <= pc ? b : c) : 0)
    }
  }
  return { w, h, type, bpp, px }
}

export function cut({ w, h, type, bpp, px }: Image, left: number, top: number, cw: number, ch: number, k: number): Buffer {
  const [l, t] = [Math.max(0, Math.min(w - 1, left)), Math.max(0, Math.min(h - 1, top))]
  const [rw, rh] = [Math.max(1, Math.min(w - l, cw)), Math.max(1, Math.min(h - t, ch))]
  const out = Buffer.alloc(rw * k * rh * k * bpp)
  for (let y = 0; y < rh * k; y++) for (let x = 0; x < rw * k; x++) px.copy(out, (y * rw * k + x) * bpp, ((t + Math.floor(y / k)) * w + l + Math.floor(x / k)) * bpp, ((t + Math.floor(y / k)) * w + l + Math.floor(x / k) + 1) * bpp)
  return png(rw * k, rh * k, type, bpp, out)
}

export function ring({ w, h, type, bpp, px }: Image, cx: number, cy: number, r: number): Buffer {
  const t = Math.max(2, Math.round(r / 7)), tick = r + 3 * t, len = 3 * r
  for (const [c, pad] of [[[255, 255, 255], 1], [[230, 0, 0], 0]] as [[number, number, number], number][]) {
    for (let y = Math.max(0, Math.floor(cy - tick - len - 2)); y <= Math.min(h - 1, cy + tick + len + 2); y++) for (let x = Math.max(0, Math.floor(cx - tick - len - 2)); x <= Math.min(w - 1, cx + tick + len + 2); x++) {
      const dx = Math.abs(x - cx), dy = Math.abs(y - cy)
      const on = Math.abs(Math.hypot(dx, dy) - r) <= t / 2 + pad || (dx <= t / 2 + pad && dy >= tick - pad && dy <= tick + len + pad) || (dy <= t / 2 + pad && dx >= tick - pad && dx <= tick + len + pad)
      if (on) { const i = (y * w + x) * bpp; px[i] = c[0]; px[i + 1] = c[1]; px[i + 2] = c[2]; if (bpp === 4) px[i + 3] = 255 }
    }
  }
  return png(w, h, type, bpp, px)
}

export function png(w: number, h: number, type: number, bpp: number, px: Buffer) {
  const chunk = (kind: string, data: Buffer) => {
    const head = Buffer.alloc(8), crc = Buffer.alloc(4); head.writeUInt32BE(data.length); head.write(kind, 4, 'ascii')
    crc.writeUInt32BE(crc32(Buffer.concat([head.subarray(4), data]))); return Buffer.concat([head, data, crc])
  }
  const stride = w * bpp, raw = Buffer.alloc(h * (stride + 1)), ihdr = Buffer.alloc(13)
  for (let y = 0; y < h; y++) px.copy(raw, y * (stride + 1) + 1, y * stride, (y + 1) * stride)
  ihdr.writeUInt32BE(w, 0); ihdr.writeUInt32BE(h, 4); ihdr[8] = 8; ihdr[9] = type
  return Buffer.concat([Buffer.from([137, 80, 78, 71, 13, 10, 26, 10]), chunk('IHDR', ihdr), chunk('IDAT', deflateSync(raw)), chunk('IEND', Buffer.alloc(0))])
}
