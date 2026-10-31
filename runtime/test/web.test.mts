import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import type { Call } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

type El = { id: string; role: string; name: string; url?: string; dom_id?: string; states?: string[] }

function browser(t: { after(fn: () => void): void }, o: { url: () => string; els: El[]; tree?: () => boolean; on?: boolean; focused?: string }) {
  const calls: Call[] = []
  const win = { id: '7/1', title: 'Form - Browser', role: 'window', pid: 7, active: true, app: 'Browser' }
  let on = o.on ?? true
  return daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([win])
    if (c.tool === 'desk_web') { const was = on; on = c.args.on === true; return JSON.stringify({ ok: true, was }) }
    if (c.tool === 'desk_snapshot') {
      const page = on && (o.tree?.() ?? true) ? [{ ref: 1, id: '7/2', parent: 0, depth: 1, role: 'document', name: '', states: ['showing'] }, ...o.els.map((e, i) => ({ ref: i + 2, parent: 1, depth: 2, bounds: [100 * i, 200, 80, 20], states: ['showing', ...(e.id === o.focused ? ['focused'] : [])], ...e }))] : []
      return JSON.stringify({ window: win, truncated: false, nodes: [{ ref: 0, id: '7/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] }, ...page] })
    }
    if (c.tool === 'desk_read') {
      if (c.args.id === '7/2') return JSON.stringify({ ref: 0, id: '7/2', role: 'document', name: '', states: [], url: o.url() })
      const e = o.els.find((x) => x.id === c.args.id)
      return JSON.stringify({ ref: 0, states: ['showing'], ...e })
    }
    if (c.tool === 'desk_type') return JSON.stringify({ ok: true, chars: String(c.args.text).length })
    return '{"ok":true}'
  }).then(() => calls)
}

const pressed = (calls: Call[]) => calls.filter((c) => c.tool === 'desk_press').map((c) => c.args.id)

test('a web step acts on its page whatever the query, fragment or trailing slash', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/form/?step=2#top', els: [{ id: '7/3', role: 'button', name: 'Submit' }] })
  const r = await main({ steps: [{ click: 'Submit', role: 'button', web: { url: 'https://shop.test/form' } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(pressed(calls), ['7/3'])
})

test('a web step on a page a redirect moved to another path stops as an error and clicks nothing', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/login?next=/form', els: [{ id: '7/3', role: 'button', name: 'Submit' }] })
  const r = await main({ steps: [{ click: 'Submit', role: 'button', web: { url: 'https://shop.test/form' } }] })
  assert.equal(r.ok, false)
  assert.match(String(r.error), /the recording acted on https:\/\/shop\.test\/form, and the plan's window shows https:\/\/shop\.test\/login\?next=\/form now/)
  assert.deepEqual(pressed(calls), [])
})

test('a web step takes the element with its recorded DOM id over another of the same name', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/form', els: [{ id: '7/3', role: 'button', name: 'Go', dom_id: 'search' }, { id: '7/4', role: 'button', name: 'Go', dom_id: 'pay' }] })
  const r = await main({ steps: [{ click: 'Go', role: 'button', web: { url: 'https://shop.test/form', id: 'pay' } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(pressed(calls), ['7/4'])
  assert.equal(r.trace?.[0]?.how, 'dom')
})

test('a browser with its page tree off gets it on for the plan, which leaves turning it off to the daemon', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/form', on: false, els: [{ id: '7/3', role: 'button', name: 'Submit' }] })
  const r = await main({ steps: [{ click: 'Submit', role: 'button', web: { url: 'https://shop.test/form' } }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_web').map((c) => c.args), [{ pid: 7, on: true }])
  assert.deepEqual(pressed(calls), ['7/3'])
})

test('a browser whose page tree was already on keeps it on', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/form', els: [{ id: '7/3', role: 'button', name: 'Submit' }] })
  await main({ steps: [{ click: 'Submit', role: 'button', web: { url: 'https://shop.test/form' } }] })
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_web'), [])
})

test('an app step with a url opens it in that browser and takes the window that shows it', async (t) => {
  let opened = false
  const calls = await browser(t, { url: () => opened ? 'https://shop.test/form' : 'about:blank', tree: () => opened, els: [] })
  calls.push = new Proxy(calls.push, { apply: (f, me, [c]: [Call]) => { if (c.tool === 'desk_launch') opened = true; return Reflect.apply(f, me, [c]) } })
  const r = await main({ steps: [{ app: 'Browser', url: 'https://shop.test/{{page}}' }], params: { page: 'form' } })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_launch').map((c) => c.args), [{ app: 'Browser', args: ['https://shop.test/form'] }])
})

test('a recorded type step stops where the focused field has another DOM id', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/form', focused: '7/3', els: [{ id: '7/3', role: 'textbox', name: '', dom_id: 'email' }] })
  const r = await main({ steps: [{ type: 'bob', focus: { role: 'textbox', name: '', id: 'name' }, web: { url: 'https://shop.test/form' } }] })
  assert.equal(r.ok, false)
  assert.match(String(r.error), /DOM id "name"/)
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_type'), [])
})

test('the page check reads the nodes the step acts on, after the menu bar read replaces them', { skip: process.platform !== 'darwin' }, async (t) => {
  const calls: Call[] = []
  const win = { id: '7/1', title: 'Form - Browser', role: 'window', pid: 7, active: true, app: 'Browser' }
  await daemon(t, (c) => {
    calls.push(c)
    if (c.tool === 'desk_windows') return JSON.stringify([win])
    if (c.tool === 'desk_snapshot') {
      const menus = c.args.menus === true
      const nodes = [{ ref: 0, id: '7/1', parent: -1, depth: 0, role: 'window', name: win.title, states: ['showing'] }, { ref: 1, id: menus ? '7/9' : '7/2', parent: 0, depth: 1, role: 'document', name: '', states: ['showing'] }]
      if (menus) nodes.push({ ref: 2, id: '7/3', parent: 1, depth: 2, role: 'button', name: 'Pay', states: ['showing'], bounds: [10, 10, 40, 20] } as never)
      return JSON.stringify({ window: win, truncated: false, nodes })
    }
    if (c.tool === 'desk_read') return JSON.stringify({ ref: 0, id: c.args.id, role: 'document', name: '', states: [], url: c.args.id === '7/9' ? 'https://shop.test/login' : 'https://shop.test/form' })
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ click: 'Pay', role: 'button', web: { url: 'https://shop.test/form' } }] })
  assert.match(String(r.error), /shows https:\/\/shop\.test\/login now/)
  assert.deepEqual(pressed(calls), [])
})

test('a set step takes the field with its recorded DOM id among fields of the same name', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/form', els: [{ id: '7/3', role: 'textbox', name: 'Email', dom_id: 'work' }, { id: '7/4', role: 'textbox', name: 'Email', dom_id: 'home' }] })
  await main({ steps: [{ set: 'Email', value: 'a@b.test', web: { url: 'https://shop.test/form', id: 'home' } }] })
  assert.deepEqual(calls.filter((c) => c.tool === 'desk_act').map((c) => c.args.id), ['7/4'])
})

test('a DOM id finds a same-named control past the read cap on a long form', async (t) => {
  const many = Array.from({ length: 70 }, (_, i) => ({ id: `7/${i + 10}`, role: 'button', name: `Other ${i}` }))
  const calls = await browser(t, { url: () => 'https://shop.test/form', els: [...many, { id: '7/90', role: 'button', name: 'Go', dom_id: 'a' }, { id: '7/91', role: 'button', name: 'Go', dom_id: 'b' }] })
  const r = await main({ steps: [{ click: 'Go', role: 'button', web: { url: 'https://shop.test/form', id: 'b' } }] })
  assert.equal(r.ok, true, JSON.stringify(r).slice(0, 300))
  assert.deepEqual(pressed(calls), ['7/91'])
})

test('a page that shows no document yet is waited for', async (t) => {
  const t0 = Date.now()
  const calls = await browser(t, { url: () => 'https://shop.test/form', tree: () => Date.now() - t0 > 600, els: [{ id: '7/3', role: 'button', name: 'Pay' }] })
  const r = await main({ steps: [{ click: 'Pay', role: 'button', web: { url: 'https://shop.test/form' } }] })
  assert.equal(r.ok, true, JSON.stringify(r).slice(0, 300))
  assert.deepEqual(pressed(calls), ['7/3'])
})

test('a page that never shows a document stops the web step with a stop report', async (t) => {
  const calls = await browser(t, { url: () => 'https://shop.test/form', tree: () => false, els: [{ id: '7/3', role: 'button', name: 'Pay' }] })
  const r = await main({ steps: [{ click: 'Pay', role: 'button', web: { url: 'https://shop.test/form' } }] })
  assert.match(String(r.error), /the plan's window shows no address now/)
  assert.equal((r.stopped as { step: number }).step, 0)
  assert.deepEqual(pressed(calls), [])
})
