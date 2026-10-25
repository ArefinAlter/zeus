// The agent screen: threads in the manner of T3 Code, on Zeus's rules. The agent reads, checks and proposes through
// the closed tool table that mirrors Zeus's MCP tools; refused calls are shown; anything with consequences is a card
// the person presses. In this release the answers come from a script and no model is called.

import { useState } from 'preact/hooks'

import { createThread, removeThread, renameThread, send, setAccess, STARTERS, stopThread } from '../backend/agent'
import { useStore } from '../backend/store'
import { ACCESS_INFO, ACCESS_ORDER, HUMAN_ONLY, NEVER, TOOLS, accessRank, mayCall } from '../model/agentTools.ts'
import type { AgentAccess } from '../model/types.ts'
import { fmtAgo } from '../model/units.ts'
import { setPrefs, usePrefs } from '../lib/prefs'
import { href, navigate } from '../lib/router'
import { Chip } from '../ui/components'
import { Icon, Mark } from '../ui/icons'
import { Composer } from './agent/Composer'
import { Timeline } from './agent/Timeline'

function NewThread() {
  const defaultAccess = useStore((s) => s.settings.agent.defaultAccess)
  const [access, setAcc] = useState<AgentAccess>(defaultAccess)
  const start = (text: string) => {
    const id = createThread(access)
    navigate(href('agent', id))
    void send(id, text)
  }
  return (
    <div class="agent-new">
      <div class="agent-scroll">
        <div class="hero">
          <Mark />
          <h1>What should Zeus fetch or check?</h1>
          <p class="muted">Ask for downloads, files or checks. The agent reads Zeus through the same tools an MCP client gets, and proposes. You press the buttons that change anything, and only you approve.</p>
        </div>
        <div class="starters" role="list" aria-label="Start from">
          <div class="label">Start from</div>
          {STARTERS.map((s) => (
            <button type="button" role="listitem" onClick={() => start(s.text)}>
              <Icon name="sparkle" />
              <span class="name">{s.text}</span>
              <span class="meta">{s.hint}</span>
            </button>
          ))}
        </div>
      </div>
      <Composer draftKey="new" busy={false} access={access} onAccess={setAcc} onSend={start} onStop={() => undefined} />
    </div>
  )
}

function Section({ title, children }: { title: string; children: preact.ComponentChildren }) {
  return (
    <section class="dsec">
      <h3>{title}</h3>
      {children}
    </section>
  )
}

function Details({ id }: { id: string }) {
  const t = useStore((s) => s.threads.find((x) => x.id === id))
  const enabled = useStore((s) => s.settings.agent.toolsEnabled)
  const skills = useStore((s) => s.skills)
  const [renaming, setRenaming] = useState(false)
  const [title, setTitle] = useState('')
  if (!t) return null
  const calls = t.items.filter((i) => i.kind === 'tool')
  const refused = calls.filter((i) => i.kind === 'tool' && i.state === 'denied').length
  const skill = skills.find((k) => k.id === t.skill)
  return (
    <aside class="agent-side" aria-label="Thread details">
      <Section title="Thread">
        <dl class="kvs">
          <dt>state</dt>
          <dd>
            <Chip tone={t.state === 'running' ? 'accent' : ''}>{t.state}</Chip>
          </dd>
          <dt>model</dt>
          <dd>Zeus agent, prepared answers; no model is called</dd>
          <dt>started</dt>
          <dd>{fmtAgo(t.created)}</dd>
          <dt>tool calls</dt>
          <dd>
            {calls.length}
            {refused ? `, ${refused} refused` : ''}
          </dd>
          {skill ? (
            <>
              <dt>skill</dt>
              <dd>
                <a href={href('skills', skill.id)}>{skill.name}</a>
              </dd>
            </>
          ) : null}
        </dl>
        {renaming ? (
          <form
            class="renameform"
            onSubmit={(e) => {
              e.preventDefault()
              renameThread(t.id, title)
              setRenaming(false)
            }}
          >
            <input value={title} aria-label="Thread title" maxLength={80} onInput={(e) => setTitle((e.target as HTMLInputElement).value)} />
            <button type="submit" class="btn small primary">
              Save
            </button>
          </form>
        ) : (
          <div class="dactions">
            <button type="button" class="btn small" onClick={() => (setTitle(t.title), setRenaming(true))}>
              <Icon name="rename" /> Rename
            </button>
            <button type="button" class="btn small" onClick={() => (removeThread(t.id), navigate(href('agent')))}>
              <Icon name="trash" /> Delete
            </button>
          </div>
        )}
      </Section>
      <Section title="Access in this thread">
        <div class="accessrows" role="radiogroup" aria-label="Access">
          {ACCESS_ORDER.map((a) => (
            <button type="button" role="radio" aria-checked={t.access === a} class={t.access === a ? 'on' : ''} onClick={() => setAccess(t.id, a)}>
              <span class="name">{ACCESS_INFO[a].label}</span>
              <span class="desc">{ACCESS_INFO[a].blurb}</span>
            </button>
          ))}
        </div>
      </Section>
      <Section title="Tools (Zeus MCP)">
        <ul class="toollist">
          {TOOLS.map((tool) => {
            const ok = mayCall(tool.name, t.access, enabled)
            return (
              <li class={ok.ok ? 'on' : 'off'} title={ok.ok ? tool.summary : `Closed: ${ok.reason}`}>
                <Icon name={ok.ok ? 'check' : 'lock'} />
                <span class="mono">{tool.name}</span>
                <span class="lvl">{accessRank(tool.access) === 0 ? '' : ACCESS_INFO[tool.access].label}</span>
              </li>
            )
          })}
        </ul>
      </Section>
      <Section title="Only you decide">
        <ul class="never">
          {HUMAN_ONLY.map((h) => (
            <li>
              <Icon name="hand" />
              <span>
                <strong>{h.title}.</strong> {h.desc}
              </span>
            </li>
          ))}
        </ul>
      </Section>
      <Section title="Never, at any access">
        <ul class="never">
          {NEVER.map((n) => (
            <li>
              <Icon name="shield" />
              {n}
            </li>
          ))}
        </ul>
        <a class="small-text" href={href('settings', 'agent')}>
          Instructions and tools in Settings
        </a>
      </Section>
    </aside>
  )
}

function ThreadPage({ id }: { id: string }) {
  const t = useStore((s) => s.threads.find((x) => x.id === id))
  const prefs = usePrefs()
  if (!t)
    return (
      <div class="empty">
        This thread no longer exists. <a href={href('agent')}>Start a new one</a>
      </div>
    )
  return (
    <div class={`agent-thread ${prefs.agentDetails ? 'with-side' : ''}`}>
      <div class="agent-col">
        <Timeline thread={t} />
        <Composer draftKey={t.id} busy={t.state === 'running'} access={t.access} onAccess={(a) => setAccess(t.id, a)} placeholder={`Reply. This thread has ${ACCESS_INFO[t.access].label} access.`} onSend={(text) => void send(t.id, text)} onStop={() => stopThread(t.id)} />
      </div>
      {prefs.agentDetails ? <Details id={t.id} /> : null}
    </div>
  )
}

export function Agent({ threadId }: { threadId?: string }) {
  return threadId ? <ThreadPage id={threadId} /> : <NewThread />
}

export function DetailsToggle() {
  const prefs = usePrefs()
  return (
    <button type="button" class="icon-btn" aria-pressed={prefs.agentDetails} aria-label="Thread details" title="Thread details" onClick={() => setPrefs({ agentDetails: !prefs.agentDetails })}>
      <Icon name="panelright" />
    </button>
  )
}
