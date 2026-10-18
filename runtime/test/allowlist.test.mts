import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

test('an app step the allowlist refuses escalates without starting the app', async () => {
  const calls: string[] = []
  const server = await toolhost(({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return '[]'
    if (tool === 'desk_focus') return { text: 'no window "shuttle-no-such-app"', error: true }
    if (tool === 'desk_launch') return { text: 'shuttle-no-such-app is not in the app allowlist ([safety] apps in config.toml)', error: true }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ app: 'shuttle-no-such-app' }] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate?.reason ?? '', /not in the app allowlist/, JSON.stringify(r))
  } finally {
    server.close()
  }
})

test('an app step whose launch the user declines escalates with the desk\'s reason and waits for no window', async () => {
  const calls: string[] = []
  const server = await toolhost(({ tool }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return '[]'
    if (tool === 'desk_focus') return { text: 'no window "notepad"', error: true }
    if (tool === 'desk_launch') return { text: 'declined by the user', error: true }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const started = Date.now()
    const r = await main({ steps: [{ app: 'notepad' }] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.equal(r.escalate?.reason, 'declined by the user')
    assert.ok(Date.now() - started < 2000, `${Date.now() - started} ms`)
    assert.ok(calls.indexOf('desk_launch') > 0, calls.join(' '))
  } finally {
    server.close()
  }
})
