import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon, gray } from './daemon.mts'
import { main } from '../lib/draft.mts'

const crop = gray(200, 100).toString('base64')
const keys = (text: string, app = 'TextEdit') => [...text].map((c, i) => ({ t: i, kind: 'key' as const, code: 0, text: c, mods: [], app }))
const win = (title: string, bounds: [number, number, number, number] = [100, 50, 600, 400]) => ({ title, bounds })

test('a recording becomes gui_plan steps with menus merged, typed values and window titles as params, and the recorded click', async (t) => {
  const asked: string[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'model_ask') { asked.push(String(args.prompt)); return JSON.stringify({ text: 'the Green button.', usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } }) }
    return '{"ok":true}'
  })
  const events = [
    { t: 0, kind: 'click', x: 963, y: 766, app: null, element: { role: 'generic', name: 'TextEdit' }, root: false, crop },
    { t: 1, kind: 'after', window: 'rbc.txt', role: 'window', app: 'TextEdit' },
    { t: 2, kind: 'click', x: 135, y: 14, app: 'TextEdit', element: { role: 'menu', name: 'File' }, root: false, crop },
    { t: 3, kind: 'click', x: 163, y: 48, app: 'TextEdit', element: { role: 'menuitem', name: 'New' }, root: false, crop },
    { t: 4, kind: 'after', window: 'Untitled', role: 'window', app: 'TextEdit' },
    { t: 5, kind: 'note', text: 'the body and the name change each time' },
    ...keys('b4 body'),
    { t: 6, kind: 'click', x: 135, y: 14, app: 'TextEdit', element: { role: 'menu', name: 'File' }, root: false, crop },
    { t: 7, kind: 'click', x: 164, y: 155, app: 'TextEdit', element: { role: 'menuitem', name: 'Save…' }, root: false, crop },
    { t: 8, kind: 'after', window: 'save', role: 'dialog', app: 'TextEdit' },
    ...keys('rr-b4'),
    { t: 9, kind: 'click', x: 830, y: 528, app: 'TextEdit', element: { role: 'button', name: 'Save' }, root: false, window: win('Untitled'), crop, crop_scale: 2 },
    { t: 10, kind: 'after', window: 'rr-b4', role: 'window', app: 'TextEdit' },
    { t: 11, kind: 'key', code: 13, text: 'w', mods: ['cmd'], app: 'TextEdit' },
    { t: 12, kind: 'after', window: 'rbc.txt', role: 'window', app: 'TextEdit' },
    { t: 13, kind: 'click', x: 363, y: 231, app: 'Safari', element: { role: 'document', name: 'canvas' }, root: true, window: win('canvas', [29, 31, 1240, 697]), crop },
    { t: 14, kind: 'after', window: 'Green clicked', role: 'window', app: 'Safari' },
    { t: 15, kind: 'key', secret: true, app: 'Safari' },
    { t: 16, kind: 'key', secret: true, app: 'Safari' },
    { t: 17, kind: 'key', code: 36, text: '\r', mods: [], app: 'Safari' },
    { t: 18, kind: 'skipped' },
  ]
  const d = await main({ recording: { id: 'rec-1', events } as never, params: { body: 'b4 body', name: 'rr-b4', color: 'Green' } })
  const ops = d.steps.map(({ intent: _, recorded: __, ...s }) => s)
  assert.deepEqual(ops, [
    { app: 'TextEdit' },
    { menu: 'File > New', after: 'Untitled' },
    { type: '{{body}}' },
    { menu: 'File > Save…' },
    { type: '{{name}}' },
    { click: 'Save', role: 'button', after: '{{name}}' },
    { key: 'cmd+w' },
    { click: 'the {{color}} button', after: '{{color}} clicked' },
    { type: '{{password}}' },
    { key: 'return' },
  ])
  assert.deepEqual(d.steps[5]?.recorded, { text: 'Save', crop, scale: 2, at: [730, 478], size: [600, 400] })
  assert.deepEqual(d.steps[7]?.recorded, { text: 'the Green button', crop, scale: 1, root: true, at: [334, 200], size: [1240, 697] })
  assert.deepEqual(d.params, { body: { example: 'b4 body' }, name: { example: 'rr-b4' }, color: { example: 'Green' }, password: { secret: true } })
  assert.deepEqual([d.notes, d.gaps], [[{ step: 2, text: 'the body and the name change each time' }], [10]])
  assert.equal(asked.length, 1)
})

test('a recording that starts with a click in a window opens its app at that window', async (t) => {
  await daemon(t, () => JSON.stringify({ text: 'the Green button', usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } }))
  const events = [{ t: 0, kind: 'click', x: 334, y: 231, app: 'Safari', element: { role: 'document', name: 'canvas' }, root: true, window: win('canvas'), crop }]
  const d = await main({ recording: { id: 'rec-2', events } as never })
  assert.deepEqual(d.steps[0], { app: 'Safari', title: 'canvas', intent: 'open Safari at "canvas"' })
})

test('a click into a password field becomes a click and a secret password step, and the keys after it add none', async (t) => {
  await daemon(t, () => JSON.stringify({ text: 'the password field', usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } }))
  const events = [
    { t: 0, kind: 'click', x: 85, y: 194, app: 'Safari', element: { role: 'textbox', name: '' }, root: false, secret: true, window: win('pw'), crop },
    { t: 1, kind: 'key', secret: true, app: 'Safari' },
    { t: 2, kind: 'key', code: 36, text: '\r', mods: [], app: 'Safari' },
  ]
  const d = await main({ recording: { id: 'rec-3', events } as never })
  assert.deepEqual(d.steps.map(({ intent: _, recorded: __, ...s }) => s), [{ app: 'Safari', title: 'pw' }, { click: 'the password field' }, { type: '{{password}}' }, { key: 'return' }])
  assert.deepEqual(d.params, { password: { secret: true } })
})

test('a password field that took the focus with no click becomes one secret password step', async (t) => {
  await daemon(t, () => JSON.stringify({ text: 'the user field', usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } }))
  const events = [
    { t: 0, kind: 'click', x: 60, y: 173, app: 'Safari', element: { role: 'textbox', name: '' }, root: false, window: win('pw'), crop },
    ...keys('alice', 'Safari'),
    { t: 1, kind: 'key', code: 48, text: '\t', mods: [], app: 'Safari' },
    { t: 2, kind: 'password', app: 'Safari' },
  ]
  const d = await main({ recording: { id: 'rec-4', events } as never, params: { user: 'alice' } })
  assert.deepEqual(d.steps.map(({ intent: _, recorded: __, ...s }) => s), [{ app: 'Safari', title: 'pw' }, { click: 'the user field' }, { type: '{{user}}' }, { key: 'tab' }, { type: '{{password}}' }])
})

test('a click near the screen edge keeps its crop offset for the replay', async (t) => {
  await daemon(t, () => JSON.stringify({ text: 'the user field', usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } }))
  const events = [{ t: 0, kind: 'click', x: 60, y: 173, app: 'Safari', element: { role: 'textbox', name: '' }, root: false, window: win('pw', [0, 31, 1240, 697]), crop, crop_offset: [-20, 0] }]
  const d = await main({ recording: { id: 'rec-5', events } as never })
  assert.deepEqual(d.steps[1]?.recorded, { text: 'the user field', crop, scale: 1, offset: [-20, 0], at: [60, 142], size: [1240, 697] })
})

test('each typed run and password keeps the field the recording typed into, or unverified where it read none', async (t) => {
  await daemon(t, () => JSON.stringify({ text: 'the user field', usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } }))
  const field = { role: 'textbox', name: '', frame: 'window' }
  const secure = { role: 'textbox', name: '', frame: 'window' }
  const events = [
    { t: 0, kind: 'click', x: 60, y: 173, app: 'Safari', element: { role: 'textbox', name: '' }, root: false, window: win('pw'), crop },
    { t: 1, kind: 'key', code: 0, text: 'a', mods: [], app: 'Safari', focus: field },
    { t: 2, kind: 'key', code: 0, text: 'l', mods: [], app: 'Safari' },
    { t: 3, kind: 'key', code: 48, text: '\t', mods: [], app: 'Safari' },
    { t: 4, kind: 'password', app: 'Safari', focus: secure },
    { t: 5, kind: 'click', x: 300, y: 300, app: 'Game', element: { role: 'document', name: '' }, root: true, window: win('board'), crop },
    { t: 6, kind: 'key', code: 0, text: 'x', mods: [], app: 'Game', focus: null },
  ]
  const d = await main({ recording: { id: 'rec-6', events } as never })
  const typed = d.steps.filter((s) => 'type' in s).map(({ type, focus }) => ({ type, focus }))
  assert.deepEqual(typed, [{ type: 'al', focus: field }, { type: '{{password}}', focus: secure }, { type: 'x', focus: 'unverified' }])
})

test('a note in the middle of typing keeps the run one step with its field', async (t) => {
  await daemon(t, () => '{"ok":true}')
  const field = { role: 'textbox', name: 'To', frame: 'window' }
  const events = [
    { t: 0, kind: 'key', code: 0, text: 'b', mods: [], app: 'Mail', focus: field },
    { t: 1, kind: 'note', text: 'the address changes' },
    { t: 2, kind: 'key', code: 0, text: 'o', mods: [], app: 'Mail' },
  ]
  const d = await main({ recording: { id: 'rec-7', events } as never })
  assert.deepEqual(d.steps.filter((s) => 'type' in s).map(({ type, focus }) => ({ type, focus })), [{ type: 'bo', focus: field }])
  assert.deepEqual(d.notes, [{ step: 1, text: 'the address changes' }])
})
