import assert from 'node:assert/strict'
import { test } from 'node:test'

import { daemon } from './daemon.mts'
import * as desk from '../lib/desk.mts'

test('the input helpers send a background target only when one is given', async (t) => {
  const calls: unknown[] = []
  await daemon(t, ({ tool, args }) => { calls.push([tool, args]); return '{"ok":true}' })
  await desk.key('ctrl+s')
  await desk.key('ctrl+s', { window: 'Untitled - Notepad' })
  await desk.typeText('a', { pid: 4 })
  await desk.click(10, 20, { count: 2, window: 'Inbox', pid: 8 })
  await desk.click(10, 20)
  assert.deepEqual(calls, [
    ['desk_key', { keys: 'ctrl+s' }],
    ['desk_key', { keys: 'ctrl+s', window: 'Untitled - Notepad' }],
    ['desk_type', { text: 'a', pid: 4 }],
    ['desk_click', { x: 10, y: 20, count: 2, window: 'Inbox', pid: 8 }],
    ['desk_click', { x: 10, y: 20 }],
  ])
})
