// The desktop app's agent: a real model called by the engine's Rust process with the person's own key, using the same
// thirteen tools any MCP client of Zeus gets. This module is the page's side of it: configuration of the provider
// ladder, consent, and a run, which is a request that returns an id and an event log the page reads by polling.
//
// Nothing here sees a key after the person typed it: it goes to the engine in one request and comes back only as
// "stored" and its last four characters. Nothing here can decide an approval: there is no such route.

import { clampSkillTools } from '../model/agentTools.ts'
import type { AgentEvent } from '../model/agentEvents.ts'
import { applyEvent, trimItems, untilLastUser } from '../model/agentEvents.ts'
import { call } from './remote.ts'
import { getState, setThreads } from './store.ts'
import type { AgentItem, Thread } from './threads.ts'
import { itemId, titleOf } from './threads.ts'

export type Tone = 'info' | 'warn'

export interface ProviderView {
  id: string
  name: string
  kind: string
  enabled: boolean
  base_url: string
  effective_base_url: string
  model: string
  consent: boolean
  monthly_cap_usd: number
  tools_mode: 'native' | 'json'
  price_in_per_mtok: number | null
  price_out_per_mtok: number | null
  effective_price_in_per_mtok: number
  effective_price_out_per_mtok: number
  needs_key: boolean
  local: boolean
  key: { stored: boolean; last4: string | null }
  month: { input_tokens: number; output_tokens: number; usd: number; calls: number }
}

export interface AgentConfigView {
  providers: ProviderView[]
  offline: boolean
  daily_cap_usd: number
  thread_cap_usd: number
  default_price_in_per_mtok: number
  default_price_out_per_mtok: number
  max_tokens: number
  keystore: { backend: 'os' | 'file'; initialized: boolean; unlocked: boolean }
  kinds: { kind: string; label: string; default_base_url: string; needs_key: boolean }[]
  usage: { today: { input_tokens: number; output_tokens: number; usd: number; calls: number }; daily_cap_usd: number }
}

/** The part of the configuration the page may change (no keys). */
export interface AgentConfigWrite {
  providers: {
    id: string
    name: string
    kind: string
    enabled: boolean
    base_url: string
    model: string
    consent: boolean
    monthly_cap_usd: number
    tools_mode: 'native' | 'json'
    price_in_per_mtok: number | null
    price_out_per_mtok: number | null
  }[]
  offline: boolean
  daily_cap_usd: number
  thread_cap_usd: number
  default_price_in_per_mtok: number
  default_price_out_per_mtok: number
  max_tokens: number
}

export interface PreviewView {
  provider: { id: string; name: string; kind: string; host: string; model: string; local: boolean; consent: boolean }
  blocker: { code: string; message: string } | null
  system: string
  messages: { role: string; text: string; tool_calls?: string[]; tool?: string }[]
  tools: { name: string; description: string }[]
  tool_access: string
  estimate: { system_tokens: number; tool_tokens: number; message_tokens: number; total_input_tokens: number; max_output_tokens: number }
  sends: string[]
  never_sends: string[]
}

export interface SendBody {
  thread_id: string
  provider_id: string
  model?: string
  access: string
  text?: string
  regenerate?: boolean
  settings: { tools_enabled: string[]; max_turn_tools: number; max_batch: number; metadata_only: boolean; instructions: string }
  skill: { id: string; name: string; instructions: string; tools: string[] } | null
}

const B = '/native/v1/agent'

export const agentApi = {
  config: () => call<AgentConfigView>('GET', `${B}/config`),
  putConfig: (c: AgentConfigWrite) => call<AgentConfigView>('PUT', `${B}/config`, c),
  putKey: (id: string, key: string) => call<{ stored: boolean; last4: string }>('PUT', `${B}/keys/${encodeURIComponent(id)}`, { key }),
  deleteKey: (id: string) => call<{ stored: boolean }>('DELETE', `${B}/keys/${encodeURIComponent(id)}`),
  initKeystore: (passphrase: string) => call<AgentConfigView['keystore']>('POST', `${B}/keystore/initialize`, { passphrase }),
  unlockKeystore: (passphrase: string) => call<AgentConfigView['keystore']>('POST', `${B}/keystore/unlock`, { passphrase }),
  lockKeystore: () => call<AgentConfigView['keystore']>('POST', `${B}/keystore/lock`, {}),
  test: (id: string, model?: string) => call<{ ok: boolean; code?: string; message?: string; ms?: number; model?: string; text?: string; usd?: number }>('POST', `${B}/providers/${encodeURIComponent(id)}/test`, model ? { model } : {}),
  models: (id: string) => call<{ ok: boolean; models?: string[]; code?: string; message?: string }>('POST', `${B}/providers/${encodeURIComponent(id)}/models`, {}),
  preview: (b: SendBody) => call<PreviewView>('POST', `${B}/preview`, b),
  send: (b: SendBody) => call<{ run_id: string }>('POST', `${B}/send`, b),
  events: (runId: string, after: number) => call<{ events: AgentEvent[]; next: number; done: boolean }>('POST', `${B}/runs/${encodeURIComponent(runId)}/events`, { after }),
  stop: (runId: string) => call<{ stopped: boolean }>('POST', `${B}/runs/${encodeURIComponent(runId)}/stop`, {}),
  deleteThread: (id: string) => call<{ ok: boolean }>('DELETE', `${B}/threads/${encodeURIComponent(id)}`),
  usage: (threadId?: string) => call<{ today: { usd: number; input_tokens: number; output_tokens: number; calls: number }; thread: { usd: number; input_tokens: number; output_tokens: number; calls: number } | null; thread_cap_usd: number; daily_cap_usd: number }>('POST', `${B}/usage`, threadId ? { thread_id: threadId } : {}),
}

// ---------------------------------------------------------------- the configuration, shared by the pages

let config: AgentConfigView | null = null
const configListeners = new Set<() => void>()

export function currentConfig(): AgentConfigView | null {
  return config
}

export function subscribeConfig(f: () => void): () => void {
  configListeners.add(f)
  return () => {
    configListeners.delete(f)
  }
}

export function setConfig(c: AgentConfigView): void {
  config = c
  for (const l of configListeners) l()
}

export async function refreshConfig(): Promise<AgentConfigView> {
  const c = await agentApi.config()
  setConfig(c)
  return c
}

/** The configuration as a write (what the engine stores), from a view. */
export function toWrite(c: AgentConfigView): AgentConfigWrite {
  return {
    providers: c.providers.map((p) => ({ id: p.id, name: p.name, kind: p.kind, enabled: p.enabled, base_url: p.base_url, model: p.model, consent: p.consent, monthly_cap_usd: p.monthly_cap_usd, tools_mode: p.tools_mode, price_in_per_mtok: p.price_in_per_mtok, price_out_per_mtok: p.price_out_per_mtok })),
    offline: c.offline,
    daily_cap_usd: c.daily_cap_usd,
    thread_cap_usd: c.thread_cap_usd,
    default_price_in_per_mtok: c.default_price_in_per_mtok,
    default_price_out_per_mtok: c.default_price_out_per_mtok,
    max_tokens: c.max_tokens,
  }
}

/** Why a provider cannot be used right now, in the engine's words; null when it can. */
export function providerBlocker(c: AgentConfigView, p: ProviderView): { code: string; text: string } | null {
  if (c.offline) return { code: 'offline', text: 'Offline is on' }
  if (!p.enabled) return { code: 'disabled', text: 'switched off' }
  if (!p.model.trim()) return { code: 'needs_model', text: 'no model yet' }
  if (!p.consent) return { code: 'needs_consent', text: 'not allowed yet' }
  if (p.needs_key && !p.key.stored) return { code: 'needs_key', text: 'needs a key' }
  return null
}

/** The first provider of the ladder that can be used, else the first enabled one. */
export function defaultProvider(c: AgentConfigView): ProviderView | undefined {
  return c.providers.find((p) => !providerBlocker(c, p)) ?? c.providers.find((p) => p.enabled) ?? c.providers[0]
}

// ---------------------------------------------------------------- consent: what will be sent, and permission

export interface ConsentRequest {
  threadId: string
  preview: PreviewView
  resolve: (allowed: boolean) => void
}

let consent: ConsentRequest | null = null
const consentListeners = new Set<() => void>()

export function currentConsent(): ConsentRequest | null {
  return consent
}

export function subscribeConsent(f: () => void): () => void {
  consentListeners.add(f)
  return () => {
    consentListeners.delete(f)
  }
}

function setConsent(c: ConsentRequest | null): void {
  consent = c
  for (const l of consentListeners) l()
}

/** Show what would be sent and ask. Resolves true only when the person allows the provider (and the engine stored it). */
function askConsent(threadId: string, preview: PreviewView): Promise<boolean> {
  return new Promise((resolve) => {
    setConsent({
      threadId,
      preview,
      resolve: (allowed) => {
        setConsent(null)
        resolve(allowed)
      },
    })
  })
}

/** Store the person's permission for one provider (the engine refuses to call a provider without it). */
export async function allowProvider(id: string, allowed: boolean): Promise<void> {
  const c = await agentApi.config()
  setConfig(await agentApi.putConfig({ ...toWrite(c), providers: toWrite(c).providers.map((p) => (p.id === id ? { ...p, consent: allowed } : p)) }))
}

// ---------------------------------------------------------------- runs

const active = new Map<string, string>()
const sleep = (ms: number) => new Promise((r) => setTimeout(r, ms))

function patch(id: string, f: (t: Thread) => Thread): void {
  setThreads((list) => list.map((t) => (t.id === id ? f(t) : t)))
}

function threadOf(id: string): Thread | undefined {
  return getState().threads.find((t) => t.id === id)
}

function note(id: string, text: string, tone: Tone = 'info', link?: { label: string; to: string }): void {
  patch(id, (t) => ({ ...t, updated: Date.now(), items: [...t.items, { kind: 'notice', id: itemId(), text, tone, ...(link ? { link } : {}) }] }))
}

export function isRunning(id: string): boolean {
  return active.has(id)
}

/** A thread persisted as running but with no run in this window (the app was closed): make it idle. */
export function settleOrphan(id: string): void {
  const t = threadOf(id)
  if (t && t.state === 'running' && !active.has(id)) patch(id, (x) => ({ ...x, state: 'idle', items: applyEvent(x.items, { n: 0, type: 'done', reason: 'orphan' }) }))
}

function findSkill(text: string) {
  const s = getState()
  return s.skills.find((k) => k.enabled && k.triggers.some((tr) => text.toLowerCase().includes(tr)))
}

function bodyFor(t: Thread, providerId: string, text: string | undefined, regenerate: boolean): SendBody {
  const s = getState()
  const a = s.settings.agent
  const skill = text ? findSkill(text) : t.skill ? s.skills.find((k) => k.id === t.skill) : undefined
  return {
    thread_id: t.id,
    provider_id: providerId,
    ...(t.model ? { model: t.model } : {}),
    access: t.access,
    ...(text !== undefined ? { text } : {}),
    regenerate,
    settings: { tools_enabled: a.toolsEnabled, max_turn_tools: a.maxTurnTools, max_batch: a.maxBatch, metadata_only: a.metadataOnly, instructions: a.instructions },
    skill: skill ? { id: skill.id, name: skill.name, instructions: skill.instructions, tools: clampSkillTools(skill.tools) } : null,
  }
}

/** Send a message (or answer the last one again) in a thread through the engine's agent. */
export async function sendNative(id: string, text: string | null, regenerate = false): Promise<void> {
  const t = threadOf(id)
  if (!t || t.state === 'running' || active.has(id)) return
  active.set(id, '')
  const fail = (message: string) => {
    note(id, message, 'warn')
    patch(id, (x) => ({ ...x, state: 'idle', updated: Date.now() }))
    active.delete(id)
  }
  try {
    const skill = text ? findSkill(text) : undefined
    patch(id, (x) => ({
      ...x,
      state: 'running',
      updated: Date.now(),
      title: x.items.length || regenerate ? x.title : titleOf(text ?? ''),
      ...(skill ? { skill: skill.id } : {}),
      items: regenerate ? untilLastUser(x.items) : [...x.items, { kind: 'user' as const, id: itemId(), text: text ?? '' }, ...(skill ? [{ kind: 'notice' as const, id: itemId(), text: `Using the skill ${skill.name}: it may call ${clampSkillTools(skill.tools).join(', ')}.`, tone: 'info' as const }] : [])],
    }))
    const cfg = await refreshConfig()
    const provider = (t.provider ? cfg.providers.find((p) => p.id === t.provider) : undefined) ?? defaultProvider(cfg)
    if (!provider) return fail('No model provider is set up yet. Add one in Settings, Providers, with your own key.')
    patch(id, (x) => ({ ...x, provider: provider.id }))
    const live = threadOf(id) ?? t
    const body = bodyFor(live, provider.id, regenerate ? undefined : (text ?? ''), regenerate)
    const preview = await agentApi.preview(body)
    if (preview.blocker?.code === 'needs_consent') {
      const allowed = await askConsent(id, preview)
      if (!allowed) return fail(`Nothing was sent: you did not allow Zeus to send this thread to ${provider.name}.`)
      await allowProvider(provider.id, true)
    }
    const { run_id } = await agentApi.send(body)
    active.set(id, run_id)
    await follow(id, run_id)
  } catch (e) {
    fail(e instanceof Error ? e.message : 'The agent could not start.')
    return
  }
  active.delete(id)
}

async function follow(id: string, runId: string): Promise<void> {
  let after = 0
  let failures = 0
  for (;;) {
    try {
      const r = await agentApi.events(runId, after)
      failures = 0
      if (r.events.length) {
        after = r.next
        patch(id, (t) => {
          let items: AgentItem[] = t.items
          let usage = t.usage
          for (const ev of r.events) {
            items = applyEvent(items, ev)
            if (ev.type === 'usage') usage = ev.thread
          }
          const last = r.events[r.events.length - 1]
          return { ...t, updated: Date.now(), items: last?.type === 'done' ? trimItems(items) : items, ...(usage ? { usage } : {}), ...(last?.type === 'done' ? { state: 'idle' as const } : {}) }
        })
      }
      if (r.done) break
    } catch (e) {
      failures += 1
      if (failures >= 6) {
        note(id, e instanceof Error ? `Lost the agent's answer: ${e.message}` : 'Lost the agent.', 'warn')
        patch(id, (t) => ({ ...t, state: 'idle' }))
        return
      }
    }
    await sleep(250)
  }
  patch(id, (t) => (t.state === 'running' ? { ...t, state: 'idle' } : t))
}

/** Stop the run of a thread (the answer so far stays). */
export function stopNative(id: string): void {
  const run = active.get(id)
  if (run) void agentApi.stop(run).catch(() => undefined)
}

/** Delete a thread's model transcript in the engine. */
export function forgetNative(id: string): void {
  stopNative(id)
  void agentApi.deleteThread(id).catch(() => undefined)
}

export function setProvider(id: string, providerId: string, model?: string): void {
  patch(id, (t) => ({ ...t, provider: providerId, ...(model !== undefined ? { model } : {}) }))
}
