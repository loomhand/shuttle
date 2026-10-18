import assert from 'node:assert/strict'
import { readFileSync, readdirSync } from 'node:fs'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('no gui_plan step and no desk API call reaches the daemon\'s safety controls', async () => {
  const win = { id: '1', title: 'Untitled - TextEdit', role: 'window', pid: process.pid, active: true, app: 'TextEdit' }
  const node = (id: string, role: string, name: string, depth = 1) => ({ ref: Number(id), id, parent: depth ? 0 : -1, depth, role, name, states: ['showing'], bounds: [10, 10, 20, 20] })
  const nodes = [node('0', 'window', win.title, 0), node('1', 'button', 'OK'), node('2', 'checkbox', 'Wrap'), node('3', 'textbox', 'Name'), node('4', 'menu', 'File'), node('5', 'menuitem', 'Save', 2)]
  const tools = new Set<string>()
  const server = await toolhost(({ tool, args }) => {
    tools.add(tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, nodes, truncated: false })
    if (tool === 'desk_find') return JSON.stringify({ window: win, found: nodes.filter((n) => n.name === args.name) })
    if (tool === 'desk_read') return JSON.stringify(nodes.find((n) => n.id === args.id) ?? nodes[0])
    if (tool === 'desk_at') return JSON.stringify({ found: null, window: win })
    if (tool === 'desk_focus') return JSON.stringify({ ok: true, title: win.title, pid: win.pid })
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  const desk = await import('../lib/desk.mts')
  try {
    const steps = [{ app: 'TextEdit' }, { menu: 'File > Save' }, { click: 'OK' }, { check: 'Wrap', on: true }, { set: 'Name', value: 'x' }, { type: 'x' }, { key: 'ctrl+s' }, { wait: 'Nope', timeout_ms: 50 }, { verify: 'Name', value: 'x' }, { close: true }]
    for (const step of steps) await main({ steps: [step] })
    await Promise.allSettled([desk.windows(), desk.press('1'), desk.act('1', 'click'), desk.at(1, 1), desk.key('a'), desk.typeText('a'), desk.click(1, 1), desk.focus('x'), desk.close('x'), desk.launch('x'), desk.shot({})])
    const other = [...tools].filter((t) => !/^(desk|model)_/.test(t))
    assert.deepEqual(other, [], [...tools].join(' '))
    assert.ok(tools.size >= 10, [...tools].join(' '))
    for (const f of readdirSync(new URL('../lib/', import.meta.url))) {
      assert.ok(!readFileSync(new URL(`../lib/${f}`, import.meta.url), 'utf8').includes('safety_'), f)
    }
  } finally {
    server.close()
  }
})
