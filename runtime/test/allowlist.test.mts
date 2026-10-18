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
