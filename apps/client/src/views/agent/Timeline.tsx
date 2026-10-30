// The conversation: what the person wrote, what the agent said, the tools it called (refusals included) and the cards
// that came out of them. Consecutive tool calls fold into one line that opens to each call's input and answer.

import { useEffect, useRef, useState } from 'preact/hooks'

import { agentApi } from '../../backend/agentNative'
import type { AgentItem, Thread } from '../../backend/threads'
import { Chip, CopyButton, JsonBlock } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { Markdown } from '../../ui/Markdown'
import { CardView } from './Cards'

type ToolItem = Extract<AgentItem, { kind: 'tool' }>

function ToolRow({ t }: { t: ToolItem }) {
  const [open, setOpen] = useState(false)
  const [busy, setBusy] = useState(false)
  const [sent, setSent] = useState(false)
  const [error, setError] = useState('')
  const waiting = t.state === 'waiting' && !!t.review
  const decide = async (approve: boolean) => {
    if (!t.review?.ready || busy || sent) return
    setBusy(true); setError('')
    try { await agentApi.review(t.review.runId, t.review.id, approve); setSent(true) }
    catch (error) { setError(error instanceof Error ? error.message : 'The decision could not be sent.') }
    finally { setBusy(false) }
  }
  return (
    <li class={`tool ${t.state}`}>
      <button type="button" class="toolhead" aria-expanded={open} onClick={() => setOpen((v) => !v)}>
        <span class="tstate" aria-label={t.state}>
          {t.state === 'running' ? <i class="spin" /> : t.state === 'ok' ? <Icon name="check" /> : (t.state === 'denied' || waiting) ? <Icon name="lock" /> : <Icon name="x" />}
        </span>
        <span class="mono tname">
          {t.tool}
          {t.action ? <span class="muted"> · {t.action}</span> : null}
        </span>
        <span class="tsum">{t.summary}</span>
        {t.ms !== undefined ? <span class="tms mono">{t.ms < 1000 ? `${t.ms} ms` : `${(t.ms / 1000).toFixed(1)} s`}</span> : null}
        <Icon name="chevron" />
      </button>
      {open || waiting ? (
        <div class="toolbody">
          <div class="label">Input</div>
          <JsonBlock value={t.input ?? {}} max={waiting ? Number.POSITIVE_INFINITY : 20_000} />
          {waiting && !t.review?.ready ? <p role="status">Saving this proposed action for review…</p> : null}
          {waiting && t.review?.ready ? <div class="stack" role="group" aria-label="Review proposed action">
            <p>This action has not run. Review the operations above; approving applies only to this request. Private credentials are hidden.</p>
            {sent ? <p>Decision sent. Waiting for Zeus…</p> : <div class="toolbar">
              <button type="button" class="btn" disabled={busy} onClick={() => void decide(false)}>Decline</button>
              <button type="button" class="btn primary" disabled={busy} onClick={() => void decide(true)}>Run this action</button>
            </div>}
            {error ? <p role="alert">{error}</p> : null}
          </div> : null}
          {t.output !== undefined ? (
            <>
              <div class="label">Zeus answered</div>
              <JsonBlock value={t.output} />
            </>
          ) : t.state === 'denied' ? (
            <div class="muted small">Refused before it reached Zeus: {t.summary.replace(/^refused: /, '')}.</div>
          ) : null}
        </div>
      ) : null}
    </li>
  )
}

function ToolGroup({ tools, live }: { tools: ToolItem[]; live: boolean }) {
  const running = tools.some((t) => t.state === 'running')
  const waiting = tools.some((t) => t.state === 'waiting')
  const [open, setOpen] = useState<boolean | null>(null)
  const denied = tools.filter((t) => t.state === 'denied').length
  const expanded = waiting || (open ?? (running || live || denied > 0))
  const total = tools.reduce((n, t) => n + (t.ms ?? 0), 0)
  return (
    <div class={`toolgroup ${expanded ? 'open' : ''}`}>
      <button type="button" class="grouphead" aria-expanded={expanded} onClick={() => setOpen(!expanded)}>
        <Icon name="wrench" />
        <span>
          {waiting ? 'Waiting for review' : running ? 'Working' : 'Worked'} · {tools.length} tool call{tools.length === 1 ? '' : 's'}
          {!running && total ? ` · ${(total / 1000).toFixed(1)} s` : ''}
          {denied ? ` · ${denied} refused` : ''}
        </span>
        <Icon name="chevron" />
      </button>
      {expanded ? (
        <ul>
          {tools.map((t) => (
            <ToolRow key={t.id} t={t} />
          ))}
        </ul>
      ) : null}
    </div>
  )
}

export function Timeline({ thread }: { thread: Thread }) {
  const scroller = useRef<HTMLDivElement>(null)
  const [stick, setStick] = useState(true)
  const running = thread.state === 'running'
  const last = thread.items[thread.items.length - 1]
  const lastLen = last && last.kind === 'assistant' ? last.text.length : 0

  useEffect(() => {
    const el = scroller.current
    if (el && stick) el.scrollTop = el.scrollHeight
  }, [thread.items.length, lastLen, stick, running])

  const blocks: ({ kind: 'tools'; tools: ToolItem[]; key: string } | { kind: 'item'; item: AgentItem })[] = []
  for (const item of thread.items) {
    if (item.kind === 'tool') {
      const tail = blocks[blocks.length - 1]
      if (tail && tail.kind === 'tools') tail.tools.push(item)
      else blocks.push({ kind: 'tools', tools: [item], key: item.id })
    } else blocks.push({ kind: 'item', item })
  }
  const lastBlock = blocks[blocks.length - 1]
  const workingNow = running && !(last && last.kind === 'assistant' && last.streaming) && !(lastBlock && lastBlock.kind === 'tools')

  return (
    <div class="timeline-wrap">
      <div
        class="timeline"
        ref={scroller}
        onScroll={() => {
          const el = scroller.current
          if (el) setStick(el.scrollHeight - el.scrollTop - el.clientHeight < 80)
        }}
        role="log"
        aria-live="polite"
        aria-label="Conversation"
      >
        <div class="column">
          {blocks.map((b) => {
            if (b.kind === 'tools') return <ToolGroup key={b.key} tools={b.tools} live={running && b === lastBlock} />
            const it = b.item
            switch (it.kind) {
              case 'user':
                return (
                  <div class="msg user" key={it.id}>
                    <div class="bubble">{it.text}</div>
                  </div>
                )
              case 'assistant':
                return (
                  <div class={`msg assistant ${it.streaming ? 'streaming' : ''}`} key={it.id}>
                    <Markdown text={it.text} />
                    {!it.streaming && it.text.length > 120 ? (
                      <div class="msgfoot">
                        <CopyButton text={it.text} label="Copy" />
                      </div>
                    ) : null}
                  </div>
                )
              case 'card':
                return (
                  <div class="cardrow" key={it.id}>
                    <CardView card={it.card} threadId={thread.id} itemId={it.id} />
                  </div>
                )
              case 'notice':
                return (
                  <div class={`inlinenote ${it.tone}`} key={it.id}>
                    {it.tone === 'warn' ? <Chip tone="warn">note</Chip> : null} {it.text}
                    {it.link ? (
                      <a class="notice-link" href={it.link.to}>
                        {it.link.label}
                      </a>
                    ) : null}
                  </div>
                )
              default:
                return null
            }
          })}
          {workingNow ? (
            <div class="working" aria-live="polite">
              <i class="dots" />
              <span>Working…</span>
            </div>
          ) : null}
        </div>
      </div>
      {!stick && running ? (
        <button type="button" class="jump" onClick={() => setStick(true)}>
          <Icon name="down" /> Latest
        </button>
      ) : null}
    </div>
  )
}
