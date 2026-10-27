// node --test src/model/agent.test.mjs
//
// The desktop app's agent on the page: the closed tool table against the MCP catalogue, how run events become thread
// items, and the command that opens approvals at a terminal.

import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import { test } from 'node:test'

import { TOOLS, mayCall } from './agentTools.ts'
import { applyEvent, approvalsCommand, settingsLinkFor, trimItems, untilLastUser } from './agentEvents.ts'

const catalogue = JSON.parse(readFileSync(new URL('../../../../docs/spec/generated/mcp-catalogue.json', import.meta.url), 'utf8'))

test('the page lists exactly the thirteen tools of the MCP catalogue', () => {
  assert.deepEqual(TOOLS.map((t) => t.name).sort(), catalogue.map((t) => t.name).sort())
  assert.equal(TOOLS.length, 13)
})

test('no tool, action or access level can decide an approval', () => {
  const all = TOOLS.map((t) => t.name)
  for (const name of ['zeus_approve', 'approvals_decide', 'shell', 'zeus_open']) assert.equal(mayCall(name, 'organize', all).ok, false)
  for (const action of ['approve', 'deny', 'decide', 'grant', 'release', 'unlock']) assert.equal(mayCall('zeus_control', 'organize', all, action).ok, false, action)
})

const base = []
test('events build the thread: streamed text, a tool call with its refusal, a card and a notice', () => {
  let items = base
  const ev = (e) => (items = applyEvent(items, { n: 1, ...e }))
  ev({ type: 'assistant_start', id: 'a1' })
  ev({ type: 'assistant_delta', id: 'a1', text: 'Check' })
  ev({ type: 'assistant_delta', id: 'a1', text: 'ing.' })
  ev({ type: 'assistant_end', id: 'a1' })
  ev({ type: 'tool_start', id: 't1', tool: 'zeus_enqueue', action: null, input: { urls: ['https://e.org/a'] } })
  ev({ type: 'tool_end', id: 't1', state: 'denied', summary: 'refused: it needs Download access and this thread has Read only' })
  ev({ type: 'card', id: 'c1', card: { type: 'job', id: 'job_1' } })
  ev({ type: 'notice', text: 'Offline is on.', tone: 'warn', code: 'offline' })
  assert.deepEqual(items.map((i) => i.kind), ['assistant', 'tool', 'card', 'notice'])
  assert.equal(items[0].text, 'Checking.')
  assert.equal(items[0].streaming, false)
  assert.equal(items[1].state, 'denied')
  assert.equal(items[3].link.to, '#/settings/providers')
})

test('events are idempotent for starts and cards, and a done closes what is open', () => {
  let items = []
  const ev = (e) => (items = applyEvent(items, { n: 1, ...e }))
  ev({ type: 'assistant_start', id: 'a1' })
  ev({ type: 'assistant_start', id: 'a1' })
  ev({ type: 'tool_start', id: 't1', tool: 'zeus_wait', input: {} })
  ev({ type: 'tool_start', id: 't1', tool: 'zeus_wait', input: {} })
  assert.equal(items.length, 2)
  ev({ type: 'done', reason: 'stopped' })
  assert.equal(items[0].streaming, false)
  assert.equal(items[1].state, 'error')
  assert.equal(items[1].summary, 'stopped')
})

test('answering again keeps everything up to the last user message', () => {
  const items = [
    { kind: 'user', id: 'u1', text: 'a' },
    { kind: 'assistant', id: 'a1', text: 'x' },
    { kind: 'user', id: 'u2', text: 'b' },
    { kind: 'assistant', id: 'a2', text: 'y' },
    { kind: 'card', id: 'c', card: { type: 'job', id: 'j' } },
  ]
  assert.deepEqual(untilLastUser(items).map((i) => i.id), ['u1', 'a1', 'u2'])
  assert.deepEqual(untilLastUser([{ kind: 'assistant', id: 'a', text: '' }]).length, 1)
})

test('a run that could not start points at Settings, Providers; others do not', () => {
  for (const r of ['needs_key', 'needs_consent', 'offline', 'cap', 'needs_model']) assert.equal(settingsLinkFor(r)?.section, 'providers')
  assert.equal(settingsLinkFor('stopped'), null)
  assert.equal(settingsLinkFor('rate_limited'), null)
})

test('a thread keeps its newest items when it grows', () => {
  const items = Array.from({ length: 450 }, (_, i) => ({ kind: 'user', id: `u${i}`, text: String(i) }))
  const kept = trimItems(items, 300)
  assert.equal(kept.length, 300)
  assert.equal(kept[0].id, 'u150')
})

test('the approvals command carries this app\'s environment and quotes it for the shell', () => {
  const env = { ZEUS_PROFILE: 'zeus-app', ZEUS_STATE_DIR: "C:\\Users\\O'Brien\\Zeus\\state" }
  const win = approvalsCommand(env, 'C:\\Program Files\\Zeus', 'windows')
  assert.ok(win.includes("$env:ZEUS_PROFILE='zeus-app'"))
  assert.ok(win.includes("O''Brien"))
  assert.ok(win.endsWith("& 'C:\\Program Files\\Zeus\\zeus.exe' approvals"))
  const nix = approvalsCommand({ ZEUS_PROFILE: 'zeus-app' }, '/opt/zeus app', 'linux')
  assert.equal(nix, "ZEUS_PROFILE='zeus-app' '/opt/zeus app/zeus' approvals")
})
