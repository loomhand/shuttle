import assert from 'node:assert/strict'
import { test } from 'node:test'

import { toolhost } from './toolhost.mts'

const root = (role: string, name: string) => ({ ref: 0, id: '1/1', parent: -1, depth: 0, role, name, states: ['showing'] })

function desktop() {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const dialog = { id: '2', title: 'Rename', role: 'dialog', pid: process.pid, active: false }
  const calls: string[] = []
  let value = ''
  const answer = ({ tool, args }: { tool: string; args: Record<string, unknown> }) => {
    calls.push(tool)
    const wins = dialog.active ? [doc, dialog] : [doc]
    if (tool === 'desk_windows') return JSON.stringify(wins)
    if (tool === 'desk_snapshot') {
      const w = dialog.active ? dialog : doc
      const field = { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'textbox', name: 'Name', states: ['showing', 'focused'], bounds: [10, 10, 100, 20], value }
      return JSON.stringify({ window: w, truncated: false, nodes: [root(w.role, w.title), field] })
    }
    if (tool === 'desk_key' && args.keys === 'f2') { doc.active = false; dialog.active = true }
    if (tool === 'desk_type') value += String(args.text)
    return '{"ok":true}'
  }
  return { calls, answer }
}

test('a type step into a dialog that opened mid-plan reads the dialog once before typing', async () => {
  const d = desktop()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'f2' }, { type: 'b.txt' }] })
    assert.equal(r.done, 2, JSON.stringify(r))
    const between = d.calls.slice(d.calls.indexOf('desk_key'), d.calls.indexOf('desk_type'))
    assert.equal(between.filter((t) => t === 'desk_snapshot').length, 1, between.join(' '))
  } finally {
    server.close()
  }
})

test('a step in a dialog this plan already read reads no tree when its op needs none', async () => {
  const d = desktop()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'f2' }, { type: 'b' }, { key: 'tab' }] })
    assert.equal(r.done, 3, JSON.stringify(r))
    const tab = d.calls.lastIndexOf('desk_key')
    const step = d.calls.slice(d.calls.lastIndexOf('desk_type') + 1, tab)
    assert.equal(step.filter((t) => t === 'desk_snapshot').length, 1, step.join(' '))
  } finally {
    server.close()
  }
})

test('a plan whose last step cannot open anything ends in a plain window without a tree read', async () => {
  const d = desktop()
  const server = await toolhost(d.answer)
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ wait: 'a.txt' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.ok(!d.calls.includes('desk_snapshot'), d.calls.join(' '))
  } finally {
    server.close()
  }
})

for (const shortcut of ['ctrl+shift+s', 'cmd+shift+s']) {
  test(`a typed line after ${shortcut} waits for the file chooser it opens late, and takes it as a chooser`, async () => {
    const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
    const dialog = { id: '2', title: 'Save As', role: 'dialog', pid: process.pid, active: true }
    const calls: string[] = []
    let polls = -1
    let shown = false
    const server = await toolhost(({ tool, args }) => {
      calls.push(tool === 'desk_key' ? `key ${String(args.keys)}` : tool)
      if (tool === 'desk_windows' && polls >= 0 && ++polls === 25) shown = true
      if (tool === 'desk_windows') return JSON.stringify(shown ? [{ ...doc, active: false }, dialog] : [doc])
      if (tool === 'desk_snapshot') {
        const nodes = shown
          ? [root('dialog', 'Save As'), { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'text', name: 'Save As:', states: ['showing'] }, { ref: 2, id: '2/3', parent: 0, depth: 1, role: 'textbox', name: 'File name:', states: ['showing', 'focused'] }, { ref: 3, id: '2/4', parent: 0, depth: 1, role: 'filler', name: 'File Chooser Widget', states: ['showing'] }]
          : [root('window', doc.title)]
        return JSON.stringify({ window: shown ? dialog : doc, truncated: false, nodes })
      }
      if (tool === 'desk_key' && args.keys === shortcut) polls = 0
      if (tool === 'desk_key' && /^return$/i.test(String(args.keys))) shown = false
      return '{"ok":true}'
    })
    const { main } = await import('../lib/gui_plan.mts')
    try {
      const r = await main({ steps: [{ key: shortcut }, { type: 'b.txt\n' }] })
      const typed = calls.slice(calls.indexOf(`key ${shortcut}`) + 1, calls.indexOf('desk_type'))
      assert.ok(typed.some((c) => c.startsWith('key ')), `no select key before typing into the chooser: ${typed.join(' ')}`)
      assert.equal(r.ok, true, JSON.stringify(r))
    } finally {
      server.close()
    }
  })
}

test('a plan whose last key leaves its app blocked behind a dialog it cannot see escalates instead of passing', async () => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  let blocked = false
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([doc])
    if (tool === 'desk_snapshot') return blocked ? { text: 'no accessibility answer within 3000 ms; the app may hang', error: true } : JSON.stringify({ window: doc, truncated: false, nodes: [root('window', doc.title)] })
    if (tool === 'desk_key' && args.keys === 'ctrl+e') blocked = true
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'ctrl+e' }] })
    assert.equal(r.ok, false, JSON.stringify(r))
    assert.match(r.escalate?.reason ?? '', /answers no accessibility calls/)
  } finally {
    server.close()
  }
})

test('a click in a dialog whose controls show after the step read its bare root reads it again and finds the target', async () => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const dialog = { id: '2', title: 'Confirm', role: 'dialog', pid: process.pid, active: true }
  let open = false
  let reads = 0
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(open ? [{ ...doc, active: false }, dialog] : [doc])
    if (tool === 'desk_snapshot') {
      const ok = { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'], bounds: [10, 10, 40, 20] }
      return JSON.stringify({ window: dialog, truncated: false, nodes: ++reads > 1 ? [root('dialog', 'Confirm'), ok] : [root('dialog', 'Confirm')] })
    }
    if (tool === 'desk_key' && args.keys === 'f2') open = true
    if (tool === 'desk_press') { open = false; return '{"ok":true,"how":"click"}' }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'f2' }, { click: 'OK' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
  } finally {
    server.close()
  }
})

test('a window id the plan vetted names a new window when its title changes, which the step reads again', async () => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: false }
  const front = { id: '2', title: 'Rename', role: 'dialog', pid: process.pid, active: false }
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify(front.active ? [doc, front] : [{ ...doc, active: true }])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: front, truncated: false, nodes: [root(front.title === 'Error' ? 'alert' : 'dialog', front.title)] })
    if (tool === 'desk_key' && args.keys === 'f2') front.active = true
    if (tool === 'desk_key' && args.keys === 'tab') front.title = 'Error'
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'f2' }, { key: 'tab' }, { key: 'space' }] })
    assert.match(r.escalate?.reason ?? '', /alert "Error" is in front/, JSON.stringify(r))
  } finally {
    server.close()
  }
})

test('a typed line after a shortcut keeps waiting while no window is in front, as when the focus is between the app and the chooser it opens', async () => {
  const doc = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const dialog = { id: '2', title: 'Save As', role: 'dialog', pid: process.pid, active: true }
  const calls: string[] = []
  let polls = -1
  let state: 'doc' | 'none' | 'dialog' = 'doc'
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool === 'desk_key' ? `key ${String(args.keys)}` : tool)
    if (tool === 'desk_windows' && polls >= 0) { polls++; if (polls === 2) state = 'none'; if (polls === 25) state = 'dialog' }
    if (tool === 'desk_windows') return JSON.stringify(state === 'doc' ? [doc] : state === 'none' ? [{ ...doc, active: false }] : [{ ...doc, active: false }, dialog])
    if (tool === 'desk_snapshot') {
      const nodes = state === 'dialog'
        ? [root('dialog', 'Save As'), { ref: 1, id: '2/2', parent: 0, depth: 1, role: 'text', name: 'Save As:', states: ['showing'] }, { ref: 2, id: '2/3', parent: 0, depth: 1, role: 'combobox', name: 'File name:', states: ['showing', 'focused'] }, { ref: 3, id: '2/4', parent: 0, depth: 1, role: 'filler', name: 'File Chooser Widget', states: ['showing'] }]
        : [root('window', doc.title)]
      return JSON.stringify({ window: state === 'dialog' ? dialog : doc, truncated: false, nodes })
    }
    if (tool === 'desk_key' && args.keys === 'ctrl+shift+s') polls = 0
    if (tool === 'desk_key' && /^return$/i.test(String(args.keys))) state = 'doc'
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ key: 'ctrl+shift+s' }, { type: 'b.txt\n' }] })
    const typed = calls.slice(calls.indexOf('key ctrl+shift+s') + 1, calls.indexOf('desk_type'))
    assert.ok(typed.some((c) => c.startsWith('key ')), `no select key before typing into the chooser: ${typed.join(' ')}`)
    assert.equal(r.ok, true, JSON.stringify(r))
  } finally {
    server.close()
  }
})

test('a menu step asks for the menu bar and a click step does not', async () => {
  const win = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const asked: unknown[] = []
  const nodes = [
    root('window', win.title),
    { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'], bounds: [10, 10, 40, 20] },
    { ref: 2, id: '1/3', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 3, id: '1/4', parent: 2, depth: 2, role: 'menu', name: 'File', states: ['showing'], bounds: [0, 0, 30, 20] },
    { ref: 4, id: '1/5', parent: 3, depth: 3, role: 'menuitem', name: 'Save', states: [], shortcut: 'ctrl+s' },
  ]
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') { asked.push(args.menus ?? false); return JSON.stringify({ window: win, truncated: false, nodes }) }
    if (tool === 'desk_press') return '{"ok":true,"how":"click"}'
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const click = await main({ steps: [{ click: 'OK' }] })
    assert.equal(click.ok, true, JSON.stringify(click))
    assert.ok(!asked.includes(true), JSON.stringify(asked))
    asked.length = 0
    const menu = await main({ steps: [{ menu: 'File > Save' }] })
    assert.equal(menu.ok, true, JSON.stringify(menu))
    assert.equal(asked[0], true, JSON.stringify(asked))
  } finally {
    server.close()
  }
})

test('a click on macOS whose target sits only in the menu bar reads the window again with its menu bar before any model call', { skip: process.platform !== 'darwin' }, async () => {
  const win = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const calls: string[] = []
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') {
      const bar = args.menus ? [
        { ref: 2, id: '1/3', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
        { ref: 3, id: '1/4', parent: 2, depth: 2, role: 'menu', name: 'Editor', states: ['showing'], bounds: [0, 0, 30, 20] },
        { ref: 4, id: '1/5', parent: 3, depth: 3, role: 'menuitem', name: 'Settings…', states: [], shortcut: 'cmd+,' },
      ] : []
      return JSON.stringify({ window: win, truncated: false, nodes: [root('window', win.title), { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'OK', states: ['showing'], bounds: [10, 10, 40, 20] }, ...bar] })
    }
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ click: 'Settings…' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.ok(!calls.some((c) => c.startsWith('model_') || c === 'desk_shot'), calls.join(' '))
  } finally {
    server.close()
  }
})

function menuBar(item: { name: string; shortcut?: string; states?: string[] }) {
  return [
    { ref: 10, id: '1/10', parent: 0, depth: 1, role: 'menubar', name: '', states: ['showing'] },
    { ref: 11, id: '1/11', parent: 10, depth: 2, role: 'menu', name: 'File', states: ['showing'], bounds: [0, 0, 30, 20] },
    { ref: 12, id: '1/12', parent: 11, depth: 3, role: 'menuitem', name: item.name, states: item.states ?? [], ...(item.shortcut ? { shortcut: item.shortcut } : {}) },
  ]
}

test('a verify on macOS that finds its target only in the menu bar checks it there, instead of passing on the miss', { skip: process.platform !== 'darwin' }, async () => {
  const win = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const server = await toolhost(({ tool, args }) => {
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') return JSON.stringify({ window: win, truncated: false, nodes: [root('window', win.title), ...(args.menus ? menuBar({ name: 'Print' }) : [])] })
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ verify: 'Print', enabled: false }] })
    assert.match(r.escalate?.reason ?? '', /"Print" is enabled/, JSON.stringify(r))
  } finally {
    server.close()
  }
})

test('a click on macOS that names a menu item exactly takes it over a window button whose name only contains it', { skip: process.platform !== 'darwin' }, async () => {
  const win = { id: '1', title: 'a.txt - Editor', role: 'window', pid: process.pid, active: true }
  const calls: string[] = []
  const server = await toolhost(({ tool, args }) => {
    calls.push(tool === 'desk_key' ? `key ${String(args.keys)}` : tool === 'desk_press' ? `press ${String(args.id)}` : tool)
    if (tool === 'desk_windows') return JSON.stringify([win])
    if (tool === 'desk_snapshot') {
      const button = { ref: 1, id: '1/2', parent: 0, depth: 1, role: 'button', name: 'Print Preview', states: ['showing'], bounds: [10, 10, 80, 20] }
      return JSON.stringify({ window: win, truncated: false, nodes: [root('window', win.title), button, ...(args.menus ? menuBar({ name: 'Print', shortcut: 'cmd+p' }) : [])] })
    }
    if (tool === 'desk_press') return '{"ok":true,"how":"click"}'
    return '{"ok":true}'
  })
  const { main } = await import('../lib/gui_plan.mts')
  try {
    const r = await main({ steps: [{ click: 'Print' }] })
    assert.equal(r.ok, true, JSON.stringify(r))
    assert.ok(calls.includes('press 1/12') && !calls.includes('press 1/2') && !calls.some((c) => c.startsWith('key ')), calls.join(' '))
  } finally {
    server.close()
  }
})
