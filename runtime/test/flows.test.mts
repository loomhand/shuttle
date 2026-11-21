import assert from 'node:assert/strict'
import { existsSync, mkdirSync, mkdtempSync, readFileSync, writeFileSync } from 'node:fs'
import { tmpdir } from 'node:os'
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
  const chain = (JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as Record<string, { pid: number | null }>)['']
  assert.equal(chain?.pid, 4242)
})

test('a plan marks the chain record dirty before it acts and clears the mark when it ends, so a killed run never chains over its steps', async (t) => {
  const seen: (boolean | undefined)[] = []; let watch = false
  const pend = () => (JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as Record<string, { dirty?: boolean; steps: unknown[] }>)[''] ?? { steps: [] }
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

test('a plan that starts at an untitled window of another process does not continue a chain left at an untitled window, and one of the same process does', async (t) => {
  const a = { id: 'a1', title: '', role: 'dialog', pid: 10, active: true, app: 'A', owner: 'a0' }
  const b = { id: 'b1', title: '', role: 'dialog', pid: 20, active: true, app: 'B', owner: 'b0' }
  let ws = [a]
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_windows') return JSON.stringify(ws)
    if (tool === 'desk_snapshot') return JSON.stringify({ window: ws[0], truncated: false, nodes: [{ ref: 0, id: `${ws[0]?.id}/0`, parent: -1, depth: 0, role: 'dialog', name: '', states: ['showing'] }] })
    return '{"ok":true}'
  })
  const chain = () => (JSON.parse(readFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, 'utf8')) as Record<string, { pid: number | null; steps: unknown[] }>)['']
  mkdirSync(process.env.SHUTTLE_FLOWS ?? '', { recursive: true }); writeFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, '{}')
  assert.equal((await main({ steps: [{ app: 'A' }] })).ok, true)
  ws = [b]
  assert.equal((await main({ steps: [{ key: 'shift+a' }] })).ok, true)
  assert.deepEqual([chain()?.pid, chain()?.steps], [10, [{ app: 'A' }]])
  ws = [a]
  assert.equal((await main({ steps: [{ key: 'shift+a' }] })).ok, true)
  assert.deepEqual([chain()?.pid, chain()?.steps], [10, [{ app: 'A' }, { key: 'shift+a' }]])
})

test('each caller extends and saves only its own chain, a run that names no caller shares one, and a chain written before callers were kept belongs to that one', async (t) => {
  const was = process.env.SHUTTLE_CALLER
  t.after(() => { if (was === undefined) delete process.env.SHUTTLE_CALLER; else process.env.SHUTTLE_CALLER = was })
  await daemon(t, answer(() => [notepad]))
  const as = (who: string | undefined, p: Parameters<typeof main>[0]) => { if (who === undefined) delete process.env.SHUTTLE_CALLER; else process.env.SHUTTLE_CALLER = who; return main(p) }
  const kinds = (name: string) => flow(name).steps.map((s) => s.app ?? s.key ?? s.type)
  assert.equal((await as('mcp-a', { steps: [{ app: 'Notepad' }, { type: 'memo' }] })).ok, true)
  assert.equal((await as('mcp-b', { steps: [{ key: 'ctrl+s' }] })).ok, true)
  assert.equal((await as('mcp-b', { save: 'b-flow' })).ok, false)
  assert.equal((await as('mcp-b', { steps: [{ app: 'Notepad' }, { type: 'other' }] })).ok, true)
  assert.equal((await as('mcp-a', { steps: [{ type: ' more' }] })).ok, true)
  const a = await as('mcp-a', { save: 'a-flow' })
  assert.equal(a.ok, true, JSON.stringify(a))
  assert.deepEqual(kinds('a-flow'), ['Notepad', '{{text}}', '{{text_2}}'])
  assert.deepEqual(flow('a-flow').params, { text: 'memo', text_2: ' more' })
  assert.equal((await as('mcp-b', { save: 'b-flow' })).ok, true)
  assert.deepEqual(flow('b-flow').params, { text: 'other' })
  assert.equal((await as(undefined, { steps: [{ app: 'Notepad' }, { type: 'x' }] })).ok, true)
  assert.equal((await as('', { steps: [{ key: 'ctrl+s' }] })).ok, true)
  assert.equal((await as(undefined, { save: 'shared' })).ok, true)
  assert.deepEqual(kinds('shared'), ['Notepad', '{{text}}', 'ctrl+s'])
  writeFileSync(`${process.env.SHUTTLE_FLOWS}/.pending.json`, JSON.stringify({ at: Date.now(), window: notepad.title, pid: notepad.pid, params: {}, steps: [{ app: 'Notepad' }, { type: 'old' }], dirty: false, saved: true, usage: {} }))
  assert.equal((await as('mcp-a', { save: 'taken' })).ok, false)
  assert.equal((await as(undefined, { save: 'kept' })).ok, true)
  assert.deepEqual(flow('kept').params, { text: 'old' })
})

test('a plain type whose desk reply says it went into a password field is kept out of the chain record, a saved flow, the run log and the partial result, and saving the plan again types nothing', async (t) => {
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  const partial = ((await import(`../lib/gui_plan.mts?${run}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_RUN
  const typed: unknown[] = []; let guarded = true
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_type') { typed.push(args.text); return JSON.stringify({ ok: true, ...(guarded ? { protected: true } : {}) }) }
    return answer(() => [notepad])({ tool })
  })
  const disk = () => ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
  const steps = () => [{ app: 'Notepad' }, { type: 'hunter12' }]
  const ran = await partial({ steps: steps() })
  assert.equal(ran.ok, true, JSON.stringify(ran))
  for (const text of [JSON.stringify(ran), disk(), readFileSync(`${run}/result.json`, 'utf8')]) assert.ok(!text.includes('hunter12'), text)
  typed.length = 0
  const saved = await partial({ steps: steps(), save: 'pin' })
  assert.equal(saved.ran, false, JSON.stringify(saved))
  assert.deepEqual(typed, [])
  assert.deepEqual(flow('pin').steps[1], { type: '{{text}}', secret: true })
  assert.deepEqual(flow('pin').params, {})
  guarded = false
  const plain = await partial({ steps: [{ app: 'Notepad' }, { type: 'memo12' }] })
  assert.equal(plain.ok, true, JSON.stringify(plain))
  assert.deepEqual(flow(String(plain.saved)).steps[1], { type: '{{text}}' })
  assert.deepEqual(flow(String(plain.saved)).params, { text: 'memo12' })
  assert.ok(disk().includes('memo12'), disk())
})

test('a plain line typed with its Return into a field the step\'s own read shows as a focused password field is sealed before it goes out and sent as a secret line is', async (t) => {
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  const partial = ((await import(`../lib/gui_plan.mts?${run}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_RUN
  const field = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: '', states: ['showing', 'focused', 'protected'] }
  const sent: unknown[][] = []
  for (const step of [{ type: 'hunter14\n', secret: true }, { type: 'hunter14\n' }]) {
    const shown: string[] = []; const inputs: unknown[] = []
    await daemon(t, ({ tool, args }) => {
      if (tool === 'desk_type') shown.push(readFileSync(`${run}/result.json`, 'utf8'))
      if (tool === 'desk_type' || tool === 'desk_key') inputs.push(args.text ?? args.keys)
      if (tool === 'desk_windows') return JSON.stringify([notepad])
      if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field] })
      return '{"ok":true}'
    })
    const r = await partial({ steps: [{ app: 'Notepad' }, step] })
    assert.equal(r.ok, true, JSON.stringify(r))
    const disk = ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
    assert.equal(shown.length, 1)
    for (const text of [JSON.stringify(r), disk, ...shown]) assert.ok(!text.includes('hunter14'), text)
    sent.push(inputs)
  }
  assert.deepEqual(sent, [['hunter14', 'Return'], ['hunter14', 'Return']])
})

test('a plain type whose desk call fails after finding a password field is kept out of the escalation, the partial result and the records', async (t) => {
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  const partial = ((await import(`../lib/gui_plan.mts?${run}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_RUN
  const failed = 'type_text: the device did not acknowledge the clipboard set, so the line was not pasted (into a password field)'
  await daemon(t, ({ tool }) => tool === 'desk_type' ? { text: failed, error: true } : answer(() => [notepad])({ tool }))
  const r = await partial({ steps: [{ app: 'Notepad' }, { type: 'hunter15' }] })
  assert.equal(r.escalate?.step, 1, JSON.stringify(r))
  assert.equal(r.escalate?.reason, failed)
  assert.equal(r.escalate?.outline, undefined, JSON.stringify(r))
  const disk = ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
  for (const text of [JSON.stringify(r), disk, readFileSync(`${run}/result.json`, 'utf8')]) assert.ok(!text.includes('hunter15'), text)
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
    if (tool === 'desk_type' || tool === 'desk_key') typed.push(args.text ?? args.keys)
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field] })
    return '{"ok":true}'
  })
  const ran = await main({ steps: [{ app: 'Notepad' }, { type: 'hunter7', focus: { secure: true } }] })
  assert.equal(ran.ok, true, JSON.stringify(ran))
  typed.length = 0
  const saved = await main({ steps: [{ app: 'Notepad' }, { type: 'hunter7\n', focus: { secure: true } }], save: 'login7' })
  assert.notEqual(saved.ran, false, JSON.stringify(saved))
  assert.deepEqual(typed, ['hunter7', 'Return'])
})

for (const [producer, back] of [['mask characters of its length, as on macOS', (v: string) => '•'.repeat([...v].length)], ['an empty value', () => ''], ['no value, as on Android and Linux', () => undefined]] as const) {
  test(`a value set into a password field that reads back ${producer} passes, is never written to a saved flow, the chain record or the run log, and saving the plan again still types nothing`, async (t) => {
    let value: string | undefined; let lands = true
    const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'protected'], ...(value === undefined ? {} : { value }) })
    const typed: unknown[] = []
    await daemon(t, ({ tool, args }) => {
      if (tool === 'desk_type') { typed.push(args.text); value = lands ? back(String(args.text)) : '•' }
      if (tool === 'desk_windows') return JSON.stringify([notepad])
      if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field()] })
      if (tool === 'desk_read') return JSON.stringify(field())
      return '{"ok":true}'
    })
    const disk = () => ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
    const steps = () => [{ app: 'Notepad' }, { set: 'Password', value: '{{password}}' }, { key: 'Return' }]
    const ran = await main({ steps: steps(), params: { password: 'hunter8' } })
    assert.equal(ran.ok, true, JSON.stringify(ran))
    assert.deepEqual(ran.caveats?.map((c) => c.step), back('x') ? undefined : [1], JSON.stringify(ran))
    assert.ok(!disk().includes('hunter8'), disk())
    typed.length = 0
    const saved = await main({ steps: steps(), params: { password: 'hunter8' }, save: 'login8' })
    assert.equal(saved.ran, false, JSON.stringify(saved))
    assert.deepEqual(typed, [])
    assert.ok(!readFileSync(`${process.env.SHUTTLE_FLOWS}/login8.json`, 'utf8').includes('hunter8'))
    assert.deepEqual(flow('login8').steps[1], { set: 'Password', value: '{{password}}', secret: true, fp: { role: 'textbox', name: 'Password', parent: notepad.title, nth: 0 }, seen: notepad.title })
    const literal = await main({ steps: [{ app: 'Notepad' }, { set: 'Password', value: 'hunter9' }], save: 'login9' })
    assert.equal(literal.ok, true, JSON.stringify(literal))
    assert.ok(!readFileSync(`${process.env.SHUTTLE_FLOWS}/login9.json`, 'utf8').includes('hunter9'))
    assert.ok(!disk().includes('hunter9'), disk())
    lands = false
    const stopped = await main({ steps: [{ app: 'Notepad' }, { set: 'Password', value: '{{password}}' }], params: { password: 'hunter10' } })
    assert.equal(stopped.escalate?.step, 1, JSON.stringify(stopped))
    assert.equal(stopped.escalate?.reason, '"Password" does not read back the value after set')
    assert.ok(!disk().includes('hunter10') && !JSON.stringify(stopped).includes('hunter10'), disk())
  })
}

test('a value set into a password field waits out empty read-backs for its mask characters, and a field empty for the whole wait passes with a caveat that it was not checked', async (t) => {
  let after = -1; let bullets = 2
  const field = (v: string) => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'protected'], value: v })
  await daemon(t, ({ tool }) => {
    if (tool === 'desk_type') after = 0
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field('')] })
    if (tool === 'desk_read') return JSON.stringify(field(after >= 0 && after++ >= bullets ? '•••••••' : ''))
    return '{"ok":true}'
  })
  const late = await main({ steps: [{ app: 'Notepad' }, { set: 'Password', value: 'hunter2' }] })
  assert.equal(late.ok, true, JSON.stringify(late))
  assert.equal(late.caveats, undefined, JSON.stringify(late))
  assert.equal(after, bullets + 1)
  after = -1; bullets = Infinity
  const empty = await main({ steps: [{ app: 'Notepad' }, { set: 'Password', value: 'hunter2' }] })
  assert.equal(empty.ok, true, JSON.stringify(empty))
  assert.deepEqual(empty.caveats, [{ step: 1, note: 'the value was sent to the password field, and the field reads back nothing, so it was not checked: do not set it again; no later step read what it unlocks' }])
  assert.equal(after, 10)
})

test('a literal value set into a password field that reads back otherwise stops with neither the value nor the read-back in the reply or on disk', async (t) => {
  let value = ''
  const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'protected'], value })
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_type') value = `rb-${String(args.text)}`
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field()] })
    if (tool === 'desk_read') return JSON.stringify(field())
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'Notepad' }, { set: 'Password', value: 'hunter2' }] })
  assert.equal(r.escalate?.step, 1, JSON.stringify(r))
  assert.equal(r.escalate?.reason, '"Password" does not read back the value after set')
  assert.equal(r.escalate?.outline, undefined, JSON.stringify(r))
  const disk = ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
  for (const text of [JSON.stringify(r), disk]) assert.ok(!text.includes('hunter2'), text)
})

test('a set marked secret into a field that is not protected keeps its value and read-back out of the run log, the partial result, the chain record, a saved flow and the escalation', async (t) => {
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  const partial = ((await import(`../lib/gui_plan.mts?${run}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_RUN
  let value = ''; let lands = true
  const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Token', states: ['showing'], value })
  const shown: string[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_type') { value = lands ? String(args.text) : `rb-${String(args.text)}`; shown.push(readFileSync(`${run}/result.json`, 'utf8')) }
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field()] })
    if (tool === 'desk_read') return JSON.stringify(field())
    return '{"ok":true}'
  })
  const disk = () => ['.pending.json', '.runs.jsonl'].map((n) => readFileSync(`${process.env.SHUTTLE_FLOWS}/${n}`, 'utf8')).join('\n')
  const steps = [{ app: 'Notepad' }, { set: 'Token', value: 'hunter3', secret: true }]
  const ok = await partial({ steps, save: 'token' })
  assert.equal(ok.ok, true, JSON.stringify(ok))
  for (const text of [JSON.stringify(ok), disk(), ...shown, readFileSync(`${run}/result.json`, 'utf8'), readFileSync(`${process.env.SHUTTLE_FLOWS}/token.json`, 'utf8')]) assert.ok(!text.includes('hunter3'), text)
  value = ''; lands = false
  const bad = await partial({ steps })
  assert.equal(bad.escalate?.step, 1, JSON.stringify(bad))
  assert.equal(bad.escalate?.reason, '"Token" does not read back the value after set')
  assert.equal(bad.escalate?.plan_step?.value, 'the password')
  assert.equal(bad.escalate?.outline, undefined, JSON.stringify(bad))
  for (const text of [JSON.stringify(bad), disk(), ...shown, readFileSync(`${run}/result.json`, 'utf8')]) assert.ok(!text.includes('hunter3') && !text.includes('rb-'), text)
})

test('a replay fronts the window of its own read by id among two of one title, and the saved flow holds no window id', async (t) => {
  let docs: { id: string; title: string; role: string; pid: number; active: boolean; app: string }[] = []
  const focused: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([{ id: 'calc', title: 'Calculator', role: 'window', pid: 9, active: !docs.some((d) => d.active) }, ...docs])
    if (tool === 'desk_focus') { focused.push(args.id); for (const d of docs) d.active = d.id === args.id; return JSON.stringify({ ok: true, title: args.window, pid: args.pid }) }
    if (tool === 'desk_snapshot') return JSON.stringify({ window: docs[0], truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: 'Untitled', states: ['showing'] }] })
    return '{"ok":true}'
  })
  const two = (a: string, b: string) => [a, b].map((id) => ({ id, title: 'Untitled', role: 'window', pid: process.pid, active: false, app: 'TextEdit' }))
  docs = two('w-41', 'w-42')
  const first = await main({ steps: [{ app: 'TextEdit' }, { key: 'ctrl+b' }], save: 'bold-twins' })
  assert.equal(first.saved, 'bold-twins', JSON.stringify(first))
  docs = two('w-51', 'w-52')
  const replay = await main({ flow: 'bold-twins' })
  assert.equal(replay.ok, true, JSON.stringify(replay))
  assert.deepEqual(focused, ['w-41', 'w-51'])
  assert.ok(!readFileSync(`${process.env.SHUTTLE_FLOWS}/bold-twins.json`, 'utf8').includes('w-'), readFileSync(`${process.env.SHUTTLE_FLOWS}/bold-twins.json`, 'utf8'))
})

test('a flow saved with the password of a set into a password field drops it when a replay rewrites the flow', async (t) => {
  let value: string | undefined
  const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'protected'], ...(value === undefined ? {} : { value }) })
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_type') value = '•'.repeat(String(args.text).length)
    if (tool === 'desk_windows') return JSON.stringify([notepad])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field()] })
    if (tool === 'desk_read') return JSON.stringify(field())
    return '{"ok":true}'
  })
  const old = { name: 'old-login', params: { password: 'hunter11' }, steps: [{ app: 'Notepad' }, { set: 'Password', value: '{{password}}', fp: { role: 'textbox', name: 'Pass', parent: notepad.title, nth: 0 } }], idempotent: true, saved_at: '2026-11-01T00:00:00.000Z' }
  mkdirSync(String(process.env.SHUTTLE_FLOWS), { recursive: true })
  writeFileSync(`${process.env.SHUTTLE_FLOWS}/old-login.json`, JSON.stringify(old))
  const r = await main({ flow: 'old-login' })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.equal(flow('old-login').steps[1]?.secret, true, JSON.stringify(flow('old-login')))
  assert.deepEqual(flow('old-login').params, {})
})

test('a literal value set as a secret or into a password field is in no partial result, stop report or reason of the next step', async (t) => {
  const run = mkdtempSync(`${tmpdir()}/run-`)
  process.env.SHUTTLE_RUN = run
  const partial = ((await import(`../lib/gui_plan.mts?${run}`)) as { main: typeof main }).main
  delete process.env.SHUTTLE_RUN
  let value: string | undefined; let stale = false; let typed = false
  const field = () => ({ ref: 1, id: '1/2', parent: 0, depth: 1, role: 'textbox', name: 'Password', states: ['showing', 'protected'], ...(value === undefined ? {} : { value }) })
  const shown: string[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_type') { typed = true; value = '•'.repeat(String(args.text).length); shown.push(readFileSync(`${run}/result.json`, 'utf8')) }
    if (tool === 'desk_windows') return JSON.stringify([{ ...notepad, ...(stale && typed ? { stale: true } : {}) }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: notepad, truncated: false, nodes: [{ ref: 0, id: '1/1', parent: -1, depth: 0, role: 'window', name: notepad.title, states: ['showing'] }, field()] })
    if (tool === 'desk_read') return JSON.stringify(field())
    return '{"ok":true}'
  })
  for (const secret of [{ secret: true }, {}]) {
    const set = { set: 'Password', value: 'hunter2', ...secret }
    value = undefined; stale = false; typed = false
    const stopped = await partial({ steps: [{ app: 'Notepad' }, { ...set, after: 'Signed in', timeout_ms: 100 }] })
    assert.equal(stopped.stopped?.step, 1, JSON.stringify(stopped))
    value = undefined; stale = true; typed = false
    const frozen = await partial({ steps: [{ app: 'Notepad' }, set, { key: 'Return' }] })
    assert.match(frozen.escalate?.reason ?? '', /stopped answering accessibility after step 1/, JSON.stringify(frozen))
    for (const text of [JSON.stringify(stopped), JSON.stringify(frozen), ...shown, readFileSync(`${run}/result.json`, 'utf8')]) assert.ok(!text.includes('hunter2'), `${JSON.stringify(secret)} ${text}`)
  }
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

test('a chain whose auto name another caller then saves by name neither continues into that flow nor deletes it when renamed', async (t) => {
  const was = process.env.SHUTTLE_CALLER
  t.after(() => { if (was === undefined) delete process.env.SHUTTLE_CALLER; else process.env.SHUTTLE_CALLER = was })
  const pad = { ...notepad, title: 'a.txt - Scratchpad' }
  await daemon(t, answer(() => [pad]))
  const as = (who: string, p: Parameters<typeof main>[0]) => { process.env.SHUTTLE_CALLER = who; return main(p) }
  assert.equal((await as('mcp-a', { steps: [{ app: 'Scratchpad' }, { type: 'memo' }] })).saved, 'scratchpad')
  assert.equal((await as('mcp-b', { steps: [{ app: 'Scratchpad' }, { type: 'theirs' }], save: 'scratchpad' })).saved, 'scratchpad')
  const before = readFileSync(`${process.env.SHUTTLE_FLOWS}/scratchpad.json`, 'utf8')
  assert.equal((await as('mcp-a', { steps: [{ type: ' more' }] })).saved, 'scratchpad-2')
  assert.equal((await as('mcp-a', { save: 'a-flow' })).ok, true)
  assert.equal(readFileSync(`${process.env.SHUTTLE_FLOWS}/scratchpad.json`, 'utf8'), before)
  assert.deepEqual(flow('a-flow').params, { text: 'memo', text_2: ' more' })
})

test('a chain renamed right after another caller saved its auto name by name leaves that flow', async (t) => {
  const was = process.env.SHUTTLE_CALLER
  t.after(() => { if (was === undefined) delete process.env.SHUTTLE_CALLER; else process.env.SHUTTLE_CALLER = was })
  const pad = { ...notepad, title: 'a.txt - Jotpad' }
  await daemon(t, answer(() => [pad]))
  const as = (who: string, p: Parameters<typeof main>[0]) => { process.env.SHUTTLE_CALLER = who; return main(p) }
  assert.equal((await as('mcp-a', { steps: [{ app: 'Jotpad' }, { type: 'memo' }] })).saved, 'jotpad')
  assert.equal((await as('mcp-b', { steps: [{ app: 'Jotpad' }, { type: 'theirs' }], save: 'jotpad' })).saved, 'jotpad')
  const before = readFileSync(`${process.env.SHUTTLE_FLOWS}/jotpad.json`, 'utf8')
  assert.equal((await as('mcp-a', { save: 'jot-flow' })).ok, true)
  assert.equal(readFileSync(`${process.env.SHUTTLE_FLOWS}/jotpad.json`, 'utf8'), before)
  assert.deepEqual(flow('jot-flow').params, { text: 'memo' })
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

test('a flow file without saved_at or params stays out of the index, and the flows beside it still index', async () => {
  const dir = process.env.SHUTTLE_FLOWS ?? ''
  mkdirSync(dir, { recursive: true })
  writeFileSync(`${dir}/broken.json`, '{"name":"broken"}')
  writeFileSync(`${dir}/good.json`, JSON.stringify({ name: 'good', params: {}, steps: [{ app: 'Notepad' }, { key: 'ctrl+s' }], idempotent: false, saved_at: '2026-11-10T00:00:00.000Z' }))
  const r = await main({ reindex: true })
  assert.equal(r.ok, true, JSON.stringify(r))
  const index = readFileSync(`${dir}/INDEX`, 'utf8')
  assert.match(index, /^good\(\) once: key ctrl\+s$/m)
  assert.doesNotMatch(index, /broken/)
})
