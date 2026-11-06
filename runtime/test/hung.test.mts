import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import { main } from '../lib/gui_plan.mts'

test('a plan whose front app stops answering escalates within the wait, names the step before, and sends no further input', async (t) => {
  const calc = { id: '1.62/3', title: 'sales.ods — LibreOffice Calc', role: 'window', pid: 7076, active: true, app: 'soffice.bin' }
  const keys: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([keys.length ? { ...calc, stale: true } : calc])
    if (tool === 'desk_key') { keys.push(args.keys); return '{"ok":true}' }
    return '{"ok":true}'
  })
  const t0 = Date.now()
  const r = await main({ steps: [{ key: 'ctrl+shift+F5' }, { key: 'ctrl+shift+4' }, { key: 'ctrl+s' }] })
  assert.ok(Date.now() - t0 < 3000, `${Date.now() - t0} ms`)
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.step, 1)
  assert.equal(r.escalate?.reason, '"sales.ods — LibreOffice Calc" stopped answering accessibility after step 0 {"key":"ctrl+shift+F5"}: its app is busy or hung; wait and continue from step 1, or close it')
  assert.deepEqual(r.escalate?.windows, [calc.title])
  assert.deepEqual(keys, ['ctrl+shift+F5'])
})

test('a plan goes on when its front app answers again within the wait', async (t) => {
  const calc = { id: '1.62/3', title: 'sales.ods — LibreOffice Calc', role: 'window', pid: 7076, active: true, app: 'soffice.bin' }
  let lists = 0
  const keys: unknown[] = []
  await daemon(t, ({ tool, args }) => {
    if (tool === 'desk_windows') { lists += 1; return JSON.stringify([keys.length === 1 && lists < 6 ? { ...calc, stale: true } : calc]) }
    if (tool === 'desk_key') { keys.push(args.keys); lists = 0; return '{"ok":true}' }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ key: 'ctrl+shift+F5' }, { key: 'ctrl+shift+4' }] })
  assert.equal(r.ok, true, JSON.stringify(r))
  assert.deepEqual(keys, ['ctrl+shift+F5', 'ctrl+shift+4'])
})
