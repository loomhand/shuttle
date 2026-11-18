import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'
import { setTimeout as sleep } from 'node:timers/promises'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const PRESS_MS = 200
const win = { id: '1', title: 'Editor', role: 'window', pid: process.pid, active: true }

for (const [what, step] of [['a menu item click', { click: 'Save' }], ['a set that picks an option', { set: 'Size', value: 'Large' }]] as const) {
  test(`the run log counts the press time of ${what} once under act`, async (t) => {
    const nodes = [
      { ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Editor', states: ['showing'] },
      { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'menuitem', name: 'Save', states: ['showing'], bounds: [10, 10, 40, 20] },
      { ref: 2, id: '1/3', parent: 0, depth: 1, role: 'combobox', name: 'Size', states: ['showing'], bounds: [10, 40, 80, 20] },
      { ref: 3, id: '1/4', parent: 2, depth: 2, role: 'option', name: 'Large', states: ['showing'], bounds: [10, 60, 80, 20] },
    ]
    await daemon(t, async ({ tool, args }) => {
      if (tool === 'desk_windows') return JSON.stringify([win])
      if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes })
      if (tool === 'desk_press') { await sleep(PRESS_MS); return JSON.stringify({ ok: true, how: 'action' }) }
      if (tool === 'desk_read') return JSON.stringify(args.id === '1/3' ? { ...nodes[2], name: 'Large' } : nodes.find((n) => n.id === args.id))
      return '{"ok":true}'
    })
    const r = await main({ steps: [step] })
    assert.equal(r.ok, true, JSON.stringify(r))
    const runs = readFileSync(`${process.env.SHUTTLE_FLOWS}/.runs.jsonl`, 'utf8').trim().split('\n')
    const logged = (JSON.parse(runs.at(-1) ?? '') as { steps: { act?: number; ms?: number }[] }).steps[0]
    assert.ok((logged?.act ?? Infinity) <= (logged?.ms ?? 0), JSON.stringify(logged))
  })
}
