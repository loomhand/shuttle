import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'
import { main } from '../lib/gui_plan.mts'

test('an app step the allowlist refuses escalates without starting the app', async (t) => {
  await toolhost(t, ({ tool }) => {
    if (tool === 'desk_windows') return '[]'
    if (tool === 'desk_focus') return { text: 'no window "shuttle-no-such-app"', error: true }
    if (tool === 'desk_launch') return { text: 'shuttle-no-such-app is not in the app allowlist ([safety] apps in config.toml)', error: true }
    return '{"ok":true}'
  })
  const r = await main({ steps: [{ app: 'shuttle-no-such-app' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.match(r.escalate?.reason ?? '', /not in the app allowlist/, JSON.stringify(r))
})

test('an app step whose launch the user declines escalates with the desk\'s reason and waits for no window', async (t) => {
  const calls: string[] = []
  await toolhost(t, ({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return '[]'
    if (tool === 'desk_focus') return { text: 'no window "notepad"', error: true }
    if (tool === 'desk_launch') return { text: 'declined by the user', error: true }
    return '{"ok":true}'
  })
  const started = Date.now()
  const r = await main({ steps: [{ app: 'notepad' }] })
  assert.equal(r.ok, false, JSON.stringify(r))
  assert.equal(r.escalate?.reason, 'declined by the user')
  assert.ok(Date.now() - started < 2000, `${Date.now() - started} ms`)
  assert.ok(calls.indexOf('desk_launch') > 0, calls.join(' '))
})
