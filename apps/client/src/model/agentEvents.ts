// The events a run of the desktop app's agent writes (the engine's `runs/<id>/events` route) and how they become the
// items of a thread. Pure functions, tested with node --test. The page never decides anything here: a card, a refusal
// and a tool result are shown exactly as the engine reports them.

import type { AgentItem, Card } from '../backend/threads.ts'

export type AgentEvent = { n: number } & (
  | { type: 'assistant_start'; id: string }
  | { type: 'assistant_delta'; id: string; text: string }
  | { type: 'assistant_end'; id: string }
  | { type: 'tool_start'; id: string; tool: string; action?: string | null; input: unknown }
  | { type: 'tool_end'; id: string; state: 'ok' | 'denied' | 'error'; summary: string; output?: unknown; ms?: number }
  | { type: 'card'; id: string; card: Card }
  | { type: 'notice'; text: string; tone: 'info' | 'warn'; code?: string }
  | { type: 'usage'; input_tokens: number; output_tokens: number; estimated?: boolean; usd: number; thread: { input_tokens: number; output_tokens: number; usd: number; calls: number } }
  | { type: 'done'; reason: string; message?: string | null }
)

let seq = 1
const noticeId = (): string => `nn${(seq++).toString(36)}`

/** Apply one event to a thread's items. Unknown ids are ignored; the same event applied twice adds nothing twice. */
export function applyEvent(items: AgentItem[], ev: AgentEvent): AgentItem[] {
  switch (ev.type) {
    case 'assistant_start':
      if (items.some((i) => i.id === ev.id)) return items
      return [...items, { kind: 'assistant', id: ev.id, text: '', streaming: true }]
    case 'assistant_delta':
      return items.map((i) => (i.kind === 'assistant' && i.id === ev.id ? { ...i, text: i.text + ev.text } : i))
    case 'assistant_end':
      return items.map((i) => (i.kind === 'assistant' && i.id === ev.id ? { ...i, streaming: false } : i))
    case 'tool_start':
      if (items.some((i) => i.id === ev.id)) return items
      return [...items, { kind: 'tool', id: ev.id, tool: ev.tool, ...(ev.action ? { action: ev.action } : {}), input: ev.input, state: 'running', summary: 'calling…' }]
    case 'tool_end':
      return items.map((i) => (i.kind === 'tool' && i.id === ev.id ? { ...i, state: ev.state, summary: ev.summary, ...(ev.output !== undefined ? { output: ev.output } : {}), ...(ev.ms !== undefined ? { ms: ev.ms } : {}) } : i))
    case 'card':
      if (items.some((i) => i.id === ev.id)) return items
      return [...items, { kind: 'card', id: ev.id, card: ev.card }]
    case 'notice': {
      const link = ev.code ? settingsLinkFor(ev.code) : null
      return [...items, { kind: 'notice', id: noticeId(), text: ev.text, tone: ev.tone, ...(link ? { link: { label: link.label, to: `#/settings/${link.section}` } } : {}) }]
    }
    case 'done':
      return items.map((i) => (i.kind === 'assistant' && i.streaming ? { ...i, streaming: false } : i.kind === 'tool' && i.state === 'running' ? { ...i, state: 'error', summary: 'stopped' } : i))
    default:
      return items
  }
}

/** The items to keep when the last answer is asked for again: everything up to and including the last user message. */
export function untilLastUser(items: AgentItem[]): AgentItem[] {
  for (let i = items.length - 1; i >= 0; i--) if (items[i]?.kind === 'user') return items.slice(0, i + 1)
  return items
}

/** What a finished run asks the page to point at: a Settings page when the run could not start. */
export function settingsLinkFor(reason: string): { label: string; section: string } | null {
  switch (reason) {
    case 'needs_key':
    case 'needs_unlock':
    case 'needs_model':
    case 'needs_consent':
    case 'disabled':
    case 'offline':
    case 'cap':
      return { label: 'Open Settings, Providers', section: 'providers' }
    default:
      return null
  }
}

/** Keep a thread's saved items bounded: the newest `max` items. */
export function trimItems(items: AgentItem[], max = 300): AgentItem[] {
  return items.length <= max ? items : items.slice(items.length - max)
}

/** The command that opens zeus's approvals on a terminal for this app's engine, per platform. */
export function approvalsCommand(env: Record<string, string>, bin: string, platform: string): string {
  const exe = platform === 'windows' ? `${bin}\\zeus.exe` : `${bin}/zeus`
  if (platform === 'windows') {
    return `${Object.entries(env).map(([k, v]) => `$env:${k}='${v.replace(/'/g, "''")}'`).join('; ')}; & '${exe.replace(/'/g, "''")}' approvals`
  }
  return `${Object.entries(env).map(([k, v]) => `${k}='${v.replace(/'/g, "'\\''")}'`).join(' ')} '${exe.replace(/'/g, "'\\''")}' approvals`
}
