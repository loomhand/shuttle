import assert from 'node:assert/strict'
import { test } from 'node:test'

import { BUILD, daemon, gray } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

type El = { id: string; role: string; name: string; states?: string[] }

function desktop(t: { after(fn: () => void): void }, els: El[], url = 'https://shop.test/form') {
  const win = { id: '7/1', title: 'Form - Browser', role: 'window', pid: 7, active: true, app: 'Browser', bounds: [0, 0, 800, 600] }
  return daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') {
      const page = [{ ref: 1, id: '7/2', parent: 0, depth: 1, role: 'document', name: '', states: ['showing'] }, ...els.map((e, i) => ({ ref: i + 2, parent: 1, depth: 2, bounds: [100 * i, 200, 80, 20], ...e, states: ['showing', ...(e.states ?? [])] }))]
      return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '7/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] }, ...page] })
    }
    if (tool === 'desk_read') return JSON.stringify(args.id === '7/2' ? { ref: 0, id: '7/2', role: 'document', name: '', states: [], url } : { ref: 0, states: ['showing'], ...els.find((e) => e.id === args.id) })
    if (tool === 'desk_screen') return JSON.stringify({ width: 800, height: 600 })
    if (tool === 'desk_shot') return { text: '{"ok":true}', image: gray(800, 600).toString('base64') }
    if (tool === 'model_ground') return { text: 'no model here', error: true }
    if (tool === 'model_decide') {
      const answers = Object.fromEntries(Object.keys(args.questions as object).map((k) => [k, { choice: 'none', confidence: 1, probabilities: { none: 1 } }]))
      return JSON.stringify({ answers, served: { model: 'm', build: BUILD, id: null }, usage: { model: 'm', input_tokens: 1, output_tokens: 1, cached_tokens: 0 } })
    }
    if (tool === 'desk_press') return JSON.stringify({ ok: true, how: 'action' })
    return '{"ok":true}'
  })
}

const rest = [{ key: 'return', intent: 'press return' }]

test('a missed after stops with the step, its intent, the reason and the steps left', async (t) => {
  await desktop(t, [{ id: '7/3', role: 'button', name: 'Pay' }])
  const r = await main({ steps: [{ click: 'Pay', role: 'button', after: 'Paid', timeout_ms: 300, intent: 'pay the order' } as never, ...rest as never[]] })
  assert.deepEqual(r.stopped, { step: 0, intent: 'pay the order', reason: r.error, remaining: [{ click: 'Pay', role: 'button', after: 'Paid', timeout_ms: 300, intent: 'pay the order' }, ...rest] })
  assert.match(String(r.error), /after this step the recording saw "Paid"/)
})

test('a focus elsewhere stops with a stop report', async (t) => {
  await desktop(t, [{ id: '7/3', role: 'textbox', name: 'Email', states: ['focused'] }])
  const r = await main({ steps: [{ type: 'bob', focus: { role: 'textbox', name: 'Name' }, intent: 'type the name' } as never, ...rest as never[]] })
  assert.equal((r.stopped as { step: number }).step, 0)
  assert.equal((r.stopped as { intent: string }).intent, 'type the name')
  assert.match(String((r.stopped as { reason: string }).reason), /the focus is on textbox "Email"/)
  assert.equal((r.stopped as { remaining: unknown[] }).remaining.length, 2)
})

test('another page stops a web step with a stop report and keeps the recorded crop out of it', async (t) => {
  await desktop(t, [{ id: '7/3', role: 'button', name: 'Pay' }], 'https://shop.test/login')
  const step = { click: 'Pay', role: 'button', web: { url: 'https://shop.test/form' }, recorded: { text: 'Pay', crop: 'AAAA' }, intent: 'pay' }
  const r = await main({ steps: [{ key: 'tab', intent: 'move on' } as never, step as never] })
  const s = r.stopped as { step: number; reason: string; remaining: Record<string, unknown>[] }
  assert.equal(s.step, 1)
  assert.match(s.reason, /the recording acted on https:\/\/shop\.test\/form/)
  assert.deepEqual(s.remaining, [{ click: 'Pay', role: 'button', web: { url: 'https://shop.test/form' }, intent: 'pay' }])
})

test('a recorded click whose fallbacks all miss stops as an error with a stop report', async (t) => {
  await desktop(t, [{ id: '7/3', role: 'button', name: 'Cancel' }])
  const r = await main({ steps: [{ click: 'Pay', recorded: { text: 'Pay now' }, intent: 'pay' } as never] })
  assert.match(String(r.error), /no element matches "Pay"; grounding failed/)
  assert.deepEqual(r.stopped, { step: 0, intent: 'pay', reason: r.error, remaining: [{ click: 'Pay', intent: 'pay' }] })
})

test('a plan that stops on no recorded check carries no stop report', async (t) => {
  await desktop(t, [])
  const r = await main({ steps: [{ click: 'Pay' }] })
  assert.equal(r.ok, false)
  assert.equal(r.stopped, undefined)
})
