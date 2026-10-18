import assert from 'node:assert/strict'
import { existsSync, readFileSync } from 'node:fs'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

const notepad = { id: '1', title: 'a.txt - Notepad', role: 'window', pid: process.pid, active: true }
const answer = (wins: () => object[]) => ({ tool }: { tool: string }) => {
  if (tool === 'desk_windows') return JSON.stringify(wins())
  if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }] })
  return '{"ok":true}'
}
const flow = (name: string) => JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/${name}.json`, 'utf8')) as { params: Record<string, string>; steps: Record<string, unknown>[] }

test('Windows paths: :base takes the file name, and a saved flow templates a title that holds a typed path\'s file name', async (t) => {
  const server = await toolhost(answer(() => [notepad]))
  t.after(() => server.close())
  const waited = await main({ steps: [{ wait: '{{file:base}} - Notepad', timeout_ms: 300 }], params: { file: 'C:\\x\\a.txt' } })
  assert.equal(waited.ok, true, JSON.stringify(waited))
  const saved = await main({ steps: [{ app: 'Notepad' }, { type: 'C:\\x\\a.txt' }, { wait: 'a.txt - Notepad' }] })
  assert.equal(saved.saved, 'notepad', JSON.stringify(saved))
  assert.equal(flow('notepad').steps[2]?.wait, '{{text:base}} - Notepad')
})

test('save renames the last flow without git and keeps the params it is given', async (t) => {
  const path = process.env.PATH
  process.env.PATH = ''
  t.after(() => { process.env.PATH = path })
  const server = await toolhost(answer(() => [notepad]))
  t.after(() => server.close())
  const first = await main({ steps: [{ app: 'Notepad' }, { type: 'hello' }] })
  assert.equal(first.saved, 'notepad', JSON.stringify(first))
  const renamed = await main({ save: 'mine', params: { extra: 'x' } })
  assert.equal(renamed.ok, true, JSON.stringify(renamed))
  assert.ok(!existsSync(`${process.env.SHUTTLE_FLOWS}/notepad.json`))
  assert.equal(flow('mine').params.extra, 'x')
})

test('a chain that ends in an untitled window keeps that window\'s process', { skip: process.platform !== 'darwin' }, async (t) => {
  const popup = { id: '9', title: '', role: 'dialog', pid: 4242, active: true }
  let keyed = false
  const server = await toolhost((c) => {
    if (c.tool === 'desk_key') keyed = true
    return answer(() => keyed ? [{ ...notepad, active: false }, popup] : [notepad])(c)
  })
  t.after(() => server.close())
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+s' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  const chain = JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as { pid: number | null }
  assert.equal(chain.pid, 4242)
})
