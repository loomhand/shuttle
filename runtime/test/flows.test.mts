import assert from 'node:assert/strict'
import { existsSync, mkdirSync, readFileSync, writeFileSync } from 'node:fs'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

const notepad = { id: '1', title: 'a.txt - Notepad', role: 'window', pid: process.pid, active: true }
const answer = (wins: () => object[]) => ({ tool }: { tool: string }) => {
  if (tool === 'desk_windows') return JSON.stringify(wins())
  if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }] })
  return '{"ok":true}'
}
const flow = (name: string) => JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/${name}.json`, 'utf8')) as { params: Record<string, string>; steps: Record<string, unknown>[] }

test('Windows paths: :base takes the file name, and a saved flow templates a title that holds a typed path\'s file name', async (t) => {
  await daemon(t, answer(() => [notepad]))
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
  await daemon(t, answer(() => [notepad]))
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
  await daemon(t, (c) => {
    if (c.tool === 'desk_key') keyed = true
    return answer(() => keyed ? [{ ...notepad, active: false }, popup] : [notepad])(c)
  })
  const r = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+s' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  const chain = JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as { pid: number | null }
  assert.equal(chain.pid, 4242)
})

test('a plan marks the chain record dirty before it acts and clears the mark when it ends, so a killed run never chains over its steps', async (t) => {
  const seen: (boolean | undefined)[] = []; let watch = false
  const pend = () => JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as { dirty?: boolean; steps: unknown[] }
  await daemon(t, (c) => {
    if (watch && c.tool === 'desk_type') seen.push(pend().dirty)
    return answer(() => [notepad])(c)
  })
  const first = await main({ steps: [{ app: 'Notepad' }, { type: 'hello' }] })
  assert.equal(first.ok, true, JSON.stringify(first))
  watch = true
  const next = await main({ steps: [{ type: ' world' }] })
  assert.equal(next.ok, true, JSON.stringify(next))
  assert.deepEqual(seen, [true])
  assert.equal(pend().dirty, false)
  assert.equal(pend().steps.length, 3)
})

test('a value typed into a password field is never written to a saved flow or the chain record', async (t) => {
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused', 'protected'] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field] })
    return '{"ok":true}'
  })
  const pending = () => readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')
  const param = await main({ steps: [{ app: 'Notepad' }, { type: '{{password}}', focus: { secure: true } }], params: { password: 'hunter2' }, save: 'login' })
  assert.equal(param.ok, true, JSON.stringify(param))
  assert.ok(!readFileSync(`${process.env.SHUTTLE_FLOWS}/login.json`, 'utf8').includes('hunter2'))
  assert.ok(!pending().includes('hunter2'), pending())
  const literal = await main({ steps: [{ app: 'Notepad' }, { type: 'hunter3', focus: { secure: true } }], save: 'login2' })
  assert.equal(literal.ok, true, JSON.stringify(literal))
  assert.ok(!readFileSync(`${process.env.SHUTTLE_FLOWS}/login2.json`, 'utf8').includes('hunter3'))
})

test('the chain record holds no password, neither from a password step that stopped nor typed as a literal', async (t) => {
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused', 'protected'] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field] })
    return '{"ok":true}'
  })
  const pending = () => readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')
  const stopped = await main({ steps: [{ app: 'Notepad' }, { type: '{{password}}', focus: { role: 'textbox', name: 'PIN', secure: true } }], params: { password: 'hunter4' } })
  assert.equal(stopped.escalate?.step, 1, JSON.stringify(stopped))
  assert.ok(!pending().includes('hunter4'), pending())
  const literal = await main({ steps: [{ app: 'Notepad' }, { type: 'hunter5\n', focus: { secure: true } }] })
  assert.equal(literal.ok, true, JSON.stringify(literal))
  assert.ok(!pending().includes('hunter5'), pending())
  const saved = await main({ save: 'login3' })
  assert.equal(saved.ok, true, JSON.stringify(saved))
  const flow = readFileSync(`${process.env.SHUTTLE_FLOWS}/login3.json`, 'utf8')
  assert.ok(!flow.includes('hunter5') && flow.includes('"type": "{{text}}\\n"'), flow)
})

test('a typed username and a typed password get distinct names whether the flow is saved from the plan or from the chain record', async (t) => {
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused', 'protected'] }
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field] })
    return '{"ok":true}'
  })
  const steps = [{ app: 'Notepad' }, { type: 'alice' }, { type: 'pw123', focus: { secure: true } }]
  const ran = await main({ steps })
  assert.equal(ran.ok, true, JSON.stringify(ran))
  const later = await main({ save: 'login4' })
  assert.equal(later.ok, true, JSON.stringify(later))
  const direct = await main({ steps, save: 'login5' })
  assert.equal(direct.ok, true, JSON.stringify(direct))
  for (const name of ['login4', 'login5']) {
    const f = flow(name)
    assert.deepEqual(f.steps.map((s) => s.type), [undefined, '{{text}}', '{{text_2}}'], `${name} ${JSON.stringify(f)}`)
    assert.deepEqual(f.params, { text: 'alice' }, name)
  }
})

test('saving the steps of a plan that just typed a password stores them without typing again', async (t) => {
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused', 'protected'] }
  const typed: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_type') typed.push(args.text)
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field] })
    return '{"ok":true}'
  })
  for (const [steps, params] of [
    [[{ app: 'Notepad' }, { type: '{{password}}', focus: { secure: true } }], { password: 'hunter6' }],
    [[{ app: 'Notepad' }, { type: 'hunter7', focus: { secure: true } }], {}],
  ] as const) {
    const ran = await main({ steps: [...steps], params })
    assert.equal(ran.ok, true, JSON.stringify(ran))
    typed.length = 0
    const saved = await main({ steps: [...steps], params, save: 'login6' })
    assert.equal(saved.ran, false, JSON.stringify(saved))
    assert.deepEqual(typed, [])
  }
})

test('saving a plan whose password step adds a Return runs it again', async (t) => {
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused', 'protected'] }
  const typed: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_type') typed.push(args.text)
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field] })
    return '{"ok":true}'
  })
  const ran = await main({ steps: [{ app: 'Notepad' }, { type: 'hunter7', focus: { secure: true } }] })
  assert.equal(ran.ok, true, JSON.stringify(ran))
  typed.length = 0
  const saved = await main({ steps: [{ app: 'Notepad' }, { type: 'hunter7\n', focus: { secure: true } }], save: 'login7' })
  assert.notEqual(saved.ran, false, JSON.stringify(saved))
  assert.deepEqual(typed, ['hunter7\n'])
})

test('find names the saved flow the model picks, and no flow under 0.6 or within 0.25 of the runner-up', async (t) => {
  let probabilities: Record<string, number> = {}
  const usage = { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 }
  await daemon(t, ({ tool }) => {
    if (tool !== 'model_decide') return '{"ok":true}'
    const [choice] = Object.entries(probabilities).sort((a, b) => b[1] - a[1])[0] ?? ['none']
    return JSON.stringify({ answers: { pick: { choice, confidence: 1, probabilities } }, served: { model: 'm', build: null, id: null }, usage })
  })
  const dir = process.env.SHUTTLE_FLOWS ?? ''
  mkdirSync(dir, { recursive: true })
  writeFileSync(`${dir}/INDEX`, 'note(n) once: app mousepad, type "replay run {{n}}"\nmail(to, subject): app Mail, type "{{to}}"\n')
  for (const [given, want] of [
    [{ f1: 0.9, f0: 0.05, none: 0.05 }, { ok: true, flow: 'mail', p: 0.9 }],
    [{ f0: 0.55, none: 0.45 }, { ok: false, flow: null, p: 0.55 }],
    [{ f0: 0.62, f1: 0.38 }, { ok: false, flow: null, p: 0.62 }],
    [{ none: 0.9, f0: 0.1 }, { ok: false, flow: null, p: 0.9 }],
  ] as const) {
    probabilities = { ...given }
    const r = await main({ find: 'send a mail' })
    assert.deepEqual({ ok: r.ok, flow: r.flow, p: r.p }, want, JSON.stringify(given))
  }
})

test('an unnamed chain never overwrites a flow saved by name under its slug, and auto saves still replace each other', async (t) => {
  await daemon(t, answer(() => [notepad]))
  const named = await main({ steps: [{ app: 'Notepad' }, { key: 'ctrl+b' }, { type: 'quarterly report' }], save: 'notepad' })
  assert.equal(named.saved, 'notepad', JSON.stringify(named))
  const before = readFileSync(`${process.env.SHUTTLE_FLOWS}/notepad.json`, 'utf8')
  const later = await main({ steps: [{ app: 'Notepad' }, { type: 'scratch' }] })
  assert.equal(later.saved, 'notepad-2', JSON.stringify(later))
  assert.equal(readFileSync(`${process.env.SHUTTLE_FLOWS}/notepad.json`, 'utf8'), before)
  const again = await main({ steps: [{ app: 'Notepad' }, { type: 'draft' }] })
  assert.equal(again.saved, 'notepad-2', JSON.stringify(again))
  assert.equal(flow('notepad-2').params.text, 'draft')
})

test('a suffixed auto name stays within the slug length, so it never names the flow it stepped around', async (t) => {
  const app = 'Supercalifragilisticexpialidocious Notebook Editor'
  const long = { id: '1', title: `a.txt - ${app}`, role: 'window', pid: process.pid, active: true }
  await daemon(t, answer(() => [long]))
  const named = await main({ steps: [{ app }, { type: 'quarterly report' }], save: app })
  const before = readFileSync(`${process.env.SHUTTLE_FLOWS}/${named.saved}.json`, 'utf8')
  const later = await main({ steps: [{ app }, { type: 'scratch' }] })
  assert.ok(typeof later.saved === 'string' && later.saved.length <= 40 && later.saved !== named.saved, JSON.stringify(later))
  assert.equal(readFileSync(`${process.env.SHUTTLE_FLOWS}/${named.saved}.json`, 'utf8'), before)
  assert.equal(flow(later.saved).params.text, 'scratch')
})
