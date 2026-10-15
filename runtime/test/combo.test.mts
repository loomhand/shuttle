import assert from 'node:assert/strict'
import { mkdtempSync } from 'node:fs'
import { createServer } from 'node:net'
import { tmpdir } from 'node:os'
import { test } from 'node:test'

test('set on a combo box without a text field picks the option from its list, does nothing when it is already chosen, and names the options when none matches', async () => {
  const prefs = { id: '1.14/322', title: 'Mousepad Preferences', role: 'dialog', pid: 889, active: true }
  const list = { id: '1.14/537', title: '', role: 'window', pid: 889, active: true }
  let open = false
  const nodes = [
    { ref: 0, id: '1.14/322', parent: -1, depth: 0, role: 'dialog', name: 'Mousepad Preferences', states: ['showing'] },
    { ref: 1, id: '1.14/400', parent: 0, depth: 1, role: 'combobox', name: 'Insert Tabs', states: ['showing'], bounds: [247, 197, 137, 34] },
    { ref: 2, id: '1.14/401', parent: 1, depth: 2, role: 'menu', name: '', states: [] },
    { ref: 3, id: '1.14/402', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Tabs', states: ['visible'] },
    { ref: 4, id: '1.14/403', parent: 2, depth: 3, role: 'menuitem', name: 'Insert Spaces', states: ['visible'] },
  ]
  const now = () => nodes.map((n) => ({ ...n, states: n.depth === 3 && open ? ['visible', 'showing'] : n.states }))
  const pressed: string[] = []
  const typed: string[] = []
  const toolhost = createServer((s) => {
    let buf = Buffer.alloc(0)
    s.on('data', (d) => {
      buf = Buffer.concat([buf, d])
      if (buf.length < 4 || buf.length < 4 + buf.readUInt32BE(0)) return
      const { tool, args } = JSON.parse(buf.subarray(4).toString())
      let text = '{"ok":true}'
      if (tool === 'desk_windows') text = JSON.stringify(open ? [list, { ...prefs, active: false }] : [prefs])
      if (tool === 'desk_snapshot') text = JSON.stringify({ window: prefs, nodes: now(), truncated: false })
      if (tool === 'desk_read') text = JSON.stringify(now().find((n) => n.id === args.id))
      if (tool === 'desk_type') typed.push(args.text)
      if (tool === 'desk_press') {
        pressed.push(args.id)
        const item = nodes.find((n) => n.id === args.id && n.role === 'menuitem')
        if (item && open) nodes[1].name = item.name
        open = args.id === '1.14/400'
        text = '{"ok":true,"how":"click"}'
      }
      const body = Buffer.from(JSON.stringify({ text, is_error: false }))
      const head = Buffer.alloc(4)
      head.writeUInt32BE(body.length)
      s.end(Buffer.concat([head, body]))
    })
  })
  const socket = `${mkdtempSync(`${tmpdir()}/gui-plan-`)}/toolhost.sock`
  await new Promise<void>((r) => toolhost.listen(socket, r))
  process.env.TOOLHOST_SOCKET = socket
  const { main } = await import('../rootfs/opt/desk/lib/gui_plan.mts')
  try {
    const set = await main({ steps: [{ set: 'Insert Tabs', value: 'Insert Spaces' }] })
    assert.equal(set.ok, true, JSON.stringify(set))
    assert.deepEqual(pressed, ['1.14/400', '1.14/403'])
    assert.equal(nodes[1].name, 'Insert Spaces')
    assert.deepEqual(typed, [])
    const again = await main({ steps: [{ set: 'Insert Spaces', value: 'Insert Spaces' }] })
    assert.equal(again.ok, true, JSON.stringify(again))
    assert.equal(again.trace[0].noop, true, JSON.stringify(again))
    assert.equal(pressed.length, 2)
    const none = await main({ steps: [{ set: 'Insert Spaces', value: 'Insert Nothing' }] })
    assert.equal(none.escalate?.reason, '"Insert Spaces" has no option "Insert Nothing"; its options: "Insert Tabs", "Insert Spaces"', JSON.stringify(none))
    assert.equal(pressed.length, 2)
  } finally {
    toolhost.close()
  }
})
