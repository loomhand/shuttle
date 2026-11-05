import assert from 'node:assert/strict'
import { test, type TestContext } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const win = { id: '1', title: 'Rows', role: 'window', pid: process.pid, active: true }

async function rows(t: TestContext, n: number) {
  const pressed: string[] = []
  const nodes = [{ ref: 0, id: '1/0', parent: -1, depth: 0, role: 'window', name: 'Rows', states: ['showing'] }, ...Array.from({ length: n }, (_, k) => ({ ref: k + 1, id: `1/${k + 1}`, parent: 0, depth: 1, role: 'button', name: 'Delete', states: ['showing'], bounds: [10, 10 + 30 * k, 60, 20] }))]
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes })
    if (tool === 'desk_press') { pressed.push(String(args.id)); return '{"ok":true,"how":"click"}' }
    if (tool.startsWith('model_')) return { text: 'no model here', error: true }
    return '{"ok":true}'
  })
  return pressed
}

test('a fingerprint picks its element by position only among as many matches as it was recorded with', async (t) => {
  const fp = { role: 'button', name: 'Delete', parent: 'Rows', nth: 1, of: 2 }
  const same = await rows(t, 2)
  const kept = await main({ steps: [{ click: 'Delete', fp }] })
  assert.equal(kept.ok, true, JSON.stringify(kept))
  assert.equal(kept.trace?.[0]?.how, 'fp')
  assert.deepEqual(same, ['1/2'])
})

test('a fingerprint whose match count changed resolves again instead of taking an element by position', async (t) => {
  const more = await rows(t, 3)
  const r = await main({ steps: [{ click: 'Delete', fp: { role: 'button', name: 'Delete', parent: 'Rows', nth: 1, of: 2 } }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.deepEqual(more, [])
})

test('a fingerprint recorded past the first of several matches never takes the one match left', async (t) => {
  await rows(t, 1)
  const r = await main({ steps: [{ click: 'Delete', fp: { role: 'button', name: 'Delete', parent: 'Rows', nth: 1 } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(r.trace?.[0]?.how, 'ax')
})
