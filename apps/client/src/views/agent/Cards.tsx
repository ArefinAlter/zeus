// The cards of an agent thread. Prose is the agent's; a card is made from what Zeus answered and carries anything with
// consequences. Its buttons are pressed by the person and named for what they do. A card of approvals has no decide
// button: it links to the Approvals page, where the facts come first.

import { useEffect, useState } from 'preact/hooks'

import { batchStart, batchToGrabber, cloudCard, organizeCard, retryCard } from '../../backend/agent'
import { agentApi } from '../../backend/agentNative'
import { NATIVE, approvalFromDaemon } from '../../backend/live'
import type { RemoteApproval } from '../../backend/remote'
import { useStore } from '../../backend/store'
import type { Card } from '../../backend/threads'
import type { Approval } from '../../model/types.ts'
import { KIND_LABEL } from '../../model/approvals.ts'
import { fmtBytes, fmtPercent, fmtRate } from '../../model/units.ts'
import { href } from '../../lib/router'
import { Chip, Notice, Progress, StateLabel } from '../../ui/components'
import type { IconName } from '../../ui/icons'
import { Icon } from '../../ui/icons'
import { jobPercent } from '../downloads/columns'
import { ApprovalCommand } from './Command'

function Shell({ icon, title, chips, children, foot }: { icon: IconName; title: string; chips?: preact.ComponentChildren; children?: preact.ComponentChildren; foot?: preact.ComponentChildren }) {
  return (
    <section class="acard">
      <header>
        <span class="well">
          <Icon name={icon} />
        </span>
        <h3>{title}</h3>
        <span class="chips">{chips}</span>
      </header>
      {children ? <div class="body">{children}</div> : null}
      {foot ? <footer>{foot}</footer> : null}
    </section>
  )
}

function BatchCard({ card, threadId, itemId }: { card: Extract<Card, { type: 'batch' }>; threadId: string; itemId: string }) {
  const queues = useStore((s) => s.queues.filter((q) => q.kind === 'download'))
  const [queue, setQueue] = useState('now')
  const [picked, setPicked] = useState<string[]>(card.items.map((i) => i.url))
  const items = card.items.filter((i) => picked.includes(i.url))
  const total = items.reduce((n, i) => n + (i.size ?? 0), 0)
  return (
    <Shell
      icon="grab"
      title={card.title}
      chips={
        <>
          <Chip>{card.items.length} files</Chip>
          {card.done === 'started' ? <Chip tone="ok">started by you</Chip> : card.done === 'grabber' ? <Chip tone="info">in the link grabber</Chip> : <Chip tone="warn">not started</Chip>}
        </>
      }
      foot={
        card.done ? (
          <a class="btn small" href={card.done === 'grabber' ? href('grabber') : href('downloads', { view: 'unfinished' })}>
            {card.done === 'grabber' ? 'Open the link grabber' : 'See them in Downloads'}
          </a>
        ) : (
          <>
            <button type="button" class="btn small" disabled={!items.length} onClick={() => batchToGrabber(threadId, itemId, items)}>
              <Icon name="grab" /> Review in the link grabber
            </button>
            <span class="spacer" />
            <select aria-label="Start now or queue" value={queue} onChange={(e) => setQueue((e.target as HTMLSelectElement).value)}>
              <option value="now">Start now</option>
              {queues.map((q) => (
                <option value={q.id}>Add to {q.name}</option>
              ))}
            </select>
            <button type="button" class="btn small primary" disabled={!items.length} onClick={() => batchStart(threadId, itemId, items, queue === 'now' ? null : queue, card.dest)}>
              <Icon name="download" /> {queue === 'now' ? `Start ${items.length} downloads` : `Queue ${items.length} downloads`}
            </button>
          </>
        )
      }
    >
      <div class="md-table">
        <table>
          <thead>
            <tr>
              {!card.done ? <th /> : null}
              <th>file</th>
              <th>size</th>
              <th>checksum</th>
            </tr>
          </thead>
          <tbody>
            {card.items.map((i) => (
              <tr>
                {!card.done ? (
                  <td>
                    <input type="checkbox" aria-label={`Include ${i.name}`} checked={picked.includes(i.url)} onChange={(e) => setPicked((p) => ((e.target as HTMLInputElement).checked ? [...p, i.url] : p.filter((x) => x !== i.url)))} />
                  </td>
                ) : null}
                <td>{i.name}</td>
                <td class="mono">{fmtBytes(i.size)}</td>
                <td class="muted">{i.expected ?? '–'}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      <div class="muted small">
        {items.length} selected · {fmtBytes(total)} · saves to <span class="mono">{card.dest}</span>
      </div>
    </Shell>
  )
}

function ApprovalsCard({ ids }: { ids: string[] }) {
  const stored = useStore((s) => s.approvals.filter((a) => ids.includes(a.id)))
  // An agent sees only the approvals it raised, so the desktop app's agent reads its own through its own connection.
  const [own, setOwn] = useState<Approval[]>([])
  useEffect(() => {
    if (!NATIVE) return undefined
    let alive = true
    const load = () =>
      agentApi.approvals(ids).then(
        (r) => alive && setOwn(r.items.map((a) => approvalFromDaemon(a as unknown as RemoteApproval))),
        () => undefined,
      )
    void load()
    const t = setInterval(load, 4000)
    return () => {
      alive = false
      clearInterval(t)
    }
  }, [ids.join(',')])
  const approvals = [...stored, ...own.filter((o) => !stored.some((s) => s.id === o.id))]
  return (
    <Shell icon="seal" title="Waiting for you" chips={<Chip tone="warn">{approvals.filter((a) => a.state === 'pending').length} pending</Chip>} foot={<><span class="muted small">Decided only by you, in the extension popup or at a terminal. Nothing in this conversation can decide one.</span><ApprovalCommand /></>}>
      <ul class="arows">
        {approvals.map((a) => (
          <li>
            {stored.some((x) => x.id === a.id) ? (
              <a href={href('approvals', a.id)}>
                <Chip tone="accent">{KIND_LABEL[a.facts.kind]}</Chip> <strong>{a.facts.website.replace(/^https?:\/\//, '')}</strong> <span class="mono">{a.bindCode}</span>
              </a>
            ) : (
              <span>
                <Chip tone="accent">{KIND_LABEL[a.facts.kind]}</Chip> <strong>{a.facts.website.replace(/^https?:\/\//, '')}</strong> <span class="mono">{a.bindCode}</span>
              </span>
            )}
            <span class="muted small">
              {a.facts.kind === 'session_grant' ? '' : `${a.facts.for} · `}
              <StateLabel state={a.state} />
            </span>
          </li>
        ))}
      </ul>
    </Shell>
  )
}

function JobCard({ id }: { id: string }) {
  const j = useStore((s) => s.jobs.find((x) => x.id === id))
  if (!j) return null
  const p = jobPercent(j)
  return (
    <Shell icon="download" title={j.name} chips={<StateLabel state={j.state} />} foot={<a class="btn small" href={href('downloads', { job: j.id })}>Open in Downloads</a>}>
      <Progress value={p ?? 0} tone={j.state === 'completed' ? 'ok' : j.state === 'failed' ? 'bad' : ''} />
      <div class="muted small">
        {fmtPercent(p)} of {fmtBytes(j.size)}
        {j.state === 'downloading' ? ` · ${fmtRate(j.rate)}` : ''} · {j.host}
      </div>
    </Shell>
  )
}

function RetryCard({ card, threadId, itemId }: { card: Extract<Card, { type: 'retry' }>; threadId: string; itemId: string }) {
  const j = useStore((s) => s.jobs.find((x) => x.id === card.jobId))
  const [err, setErr] = useState<string | null>(null)
  if (!j) return null
  return (
    <Shell
      icon="refresh"
      title={`Refresh the address of ${j.name}`}
      chips={<StateLabel state={j.state} />}
      foot={
        card.done ? (
          <span class="muted small">Pressed by you. The download continues from {fmtBytes(j.done)}.</span>
        ) : (
          <button type="button" class="btn small primary" disabled={j.state !== 'failed'} onClick={() => setErr(retryCard(threadId, itemId, j.id))}>
            <Icon name="refresh" /> Get a fresh address and retry
          </button>
        )
      }
    >
      <div class="muted small">
        {fmtBytes(j.done)} of {fmtBytes(j.size)} saved · {j.error?.code ?? 'no error'}
      </div>
      {err ? <Notice kind="error">{err}</Notice> : null}
    </Shell>
  )
}

function OrganizeCard({ card, threadId, itemId }: { card: Extract<Card, { type: 'organize' }>; threadId: string; itemId: string }) {
  const ap = useStore((s) => (card.approvalId ? s.approvals.find((a) => a.id === card.approvalId) : undefined))
  return (
    <Shell
      icon="move"
      title={`Move ${card.moves.length} files by type`}
      chips={ap ? <StateLabel state={ap.state} /> : <Chip>preview</Chip>}
      foot={
        ap ? (
          <a class="btn small" href={href('approvals', ap.id)}>
            {ap.state === 'pending' ? `Review the request (${ap.bindCode})` : 'See the request'}
          </a>
        ) : (
          <button type="button" class="btn small primary" onClick={() => organizeCard(threadId, itemId, card.moves)}>
            <Icon name="seal" /> Ask me to review this move
          </button>
        )
      }
    >
      <div class="md-table">
        <table>
          <thead>
            <tr>
              <th>file</th>
              <th>from</th>
              <th>to</th>
            </tr>
          </thead>
          <tbody>
            {card.moves.map((m) => (
              <tr>
                <td>{m.name}</td>
                <td class="mono small">{m.fromDir.replace('D:\\Downloads\\Zeus', '…')}</td>
                <td class="mono small">{m.toDir.replace('D:\\Downloads\\Zeus', '…')}</td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Shell>
  )
}

function CloudCard({ card, threadId, itemId }: { card: Extract<Card, { type: 'cloud' }>; threadId: string; itemId: string }) {
  const ap = useStore((s) => (card.approvalId ? s.approvals.find((a) => a.id === card.approvalId) : undefined))
  return (
    <Shell
      icon="cloud"
      title={`Deliver to ${card.destination}`}
      chips={ap ? <StateLabel state={ap.state} /> : <Chip>plan</Chip>}
      foot={
        ap ? (
          <a class="btn small" href={href('approvals', ap.id)}>
            See the request ({ap.bindCode})
          </a>
        ) : (
          <button type="button" class="btn small primary" onClick={() => cloudCard(threadId, itemId, card.files, card.destination)}>
            <Icon name="seal" /> Submit for my review
          </button>
        )
      }
    >
      <div class="muted small">
        {card.mode} · {card.files.map((f) => `${f.name} (${fmtBytes(f.size)})`).join(', ')}
      </div>
      <div class="muted small">Approved only at a terminal with the bind code typed in. Nothing is uploaded before that.</div>
    </Shell>
  )
}

function ProbeCard({ card }: { card: Extract<Card, { type: 'probe' }> }) {
  return (
    <Shell icon="search" title="What the server said" chips={card.login ? <Chip tone="warn">needs a login</Chip> : <Chip tone="ok">{card.status}</Chip>}>
      <dl class="kvs">
        <dt>address</dt>
        <dd class="mono small break">{card.url}</dd>
        <dt>status</dt>
        <dd>{card.status}</dd>
        <dt>type</dt>
        <dd>{card.contentType}</dd>
        <dt>resumable</dt>
        <dd>{card.resumable ? 'yes' : 'unknown'}</dd>
      </dl>
    </Shell>
  )
}

function TableCard({ card }: { card: Extract<Card, { type: 'table' }> }) {
  return (
    <Shell icon="check" title={card.title} chips={<Chip>{card.rows.length} rows</Chip>} foot={card.source ? <span class="muted small">From {card.source}</span> : null}>
      <div class="md-table">
        <table>
          <thead>
            <tr>
              {card.columns.map((c) => (
                <th>{c}</th>
              ))}
            </tr>
          </thead>
          <tbody>
            {card.rows.map((r) => (
              <tr>
                {r.map((c) => (
                  <td class="mono">{c}</td>
                ))}
              </tr>
            ))}
          </tbody>
        </table>
      </div>
    </Shell>
  )
}

export function CardView({ card, threadId, itemId }: { card: Card; threadId: string; itemId: string }) {
  switch (card.type) {
    case 'batch':
      return <BatchCard card={card} threadId={threadId} itemId={itemId} />
    case 'approvals':
      return <ApprovalsCard ids={card.ids} />
    case 'job':
      return <JobCard id={card.id} />
    case 'retry':
      return <RetryCard card={card} threadId={threadId} itemId={itemId} />
    case 'organize':
      return <OrganizeCard card={card} threadId={threadId} itemId={itemId} />
    case 'cloud':
      return <CloudCard card={card} threadId={threadId} itemId={itemId} />
    case 'probe':
      return <ProbeCard card={card} />
    case 'table':
      return <TableCard card={card} />
    default:
      return null
  }
}
