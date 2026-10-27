// Approvals: a trust surface, after Zeus ADR 0007 and spec 07. Facts come first, in a fixed order (website, asked by,
// for, lasts), from what the backend computed, never from an agent's words; the requester's own words sit below in a
// separate block. Approve and deny have equal size, nothing is focused by default, approve buttons arm 750 ms after
// the facts are painted and again after any change, a typed-code confirmation needs the bind code, and the decision
// binds to the request hash the person saw. No ornament, no animation, nothing automatic.

import { useEffect, useRef, useState } from 'preact/hooks'

import type { DecideResult } from '../backend/store'
import { LIVE, NATIVE } from '../backend/live'
import { decide, useStore } from '../backend/store'
import { ARMING_MS, KIND_LABEL, SCOPE_BUTTON, SCOPE_LASTS, codeMatches } from '../model/approvals.ts'
import type { Approval, ApprovalState, Scope } from '../model/types.ts'
import { fmtBytes, fmtStamp, fmtWhen } from '../model/units.ts'
import { href } from '../lib/router'
import { Chip, CodeCopy, Empty, Notice, PageHead, Provenance, StateLabel } from '../ui/components'

const STATES: ApprovalState[] = ['pending', 'approved', 'denied', 'expired']

export function Approvals({ state }: { state?: string }) {
  const approvals = useStore((s) => s.approvals)
  const now = useStore((s) => s.now)
  const filter = (STATES as string[]).includes(state ?? '') ? (state as ApprovalState) : 'pending'
  const rows = approvals.filter((a) => (filter === 'denied' ? a.state === 'denied' || a.state === 'canceled' : a.state === filter))
  return (
    <div class="page">
      <PageHead title="Approvals" sub="Requests that only a person decides: using a site login, handing over a browser download, running a plan, moving files, delivering to a cloud account." />
      <div class="toolbar">
        {STATES.map((s) => (
          <a class={`btn small ${s === filter ? 'primary' : ''}`} href={href('approvals', { state: s })}>
            {s} {s === 'pending' ? `(${approvals.filter((a) => a.state === 'pending').length})` : ''}
          </a>
        ))}
      </div>
      {rows.length ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>Website</th>
                <th>Asked by</th>
                <th>For</th>
                <th>Kind</th>
                <th>State</th>
                <th>Bind code</th>
                <th>{filter === 'pending' ? 'Expires' : 'Decided'}</th>
              </tr>
            </thead>
            <tbody>
              {rows.map((a) => (
                <tr>
                  <td>
                    <a href={href('approvals', a.id)}>{a.facts.website.replace(/^https?:\/\//, '')}</a>
                  </td>
                  <td>
                    {a.facts.requester.label}
                    {a.heldBy ? <span class="muted small"> · {a.heldBy === 'zeusd' ? 'held by zeusd' : NATIVE ? 'held by this app' : 'held by the pre-alpha service'}</span> : null}
                  </td>
                  <td>{a.facts.for}</td>
                  <td>{KIND_LABEL[a.facts.kind]}</td>
                  <td>
                    <StateLabel state={a.state} />
                  </td>
                  <td class="mono">{a.bindCode}</td>
                  <td>{a.decided ? `${fmtWhen(a.decided.at, now)} · ${a.decided.via}` : fmtWhen(Date.parse(a.facts.expires_at), now)}</td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <Empty>No {filter} requests.</Empty>
      )}
      <Provenance>
        {NATIVE
          ? "Requests held by zeusd (a browser login lent to a download, a handoff, a destructive file operation) are listed here and decided only on zeusd's trust surfaces: zeus approvals in a terminal, or the browser extension's popup. This page cannot decide them. Site-grabber plans proposed in this window are held by this app, which checks the request hash, the arming delay and the offered scopes before it records your decision; they ask for nothing from zeusd until you approve."
          : LIVE
          ? 'Requests held by zeusd are listed through zeus-web-bridge and decided only on zeusd\'s trust surfaces (zeus approvals in a terminal, the extension popup); this page cannot decide them. Site-grabber plans and file moves proposed in this client are held by the pre-alpha service, which checks the request hash, the arming delay and the offered scopes before it records your decision.'
          : 'zeusd decides approvals from the terminal (zeus approvals) and the extension popup, with channel evidence. A trusted desktop decision channel is planned; in this release a decision here changes only the sample data.'}
      </Provenance>
    </div>
  )
}

/** The facts, always in the same order and the same words. */
function Facts({ a }: { a: Approval }) {
  const f = a.facts
  const lasts = f.allowed_scopes.length ? f.allowed_scopes.map((s) => SCOPE_LASTS[s]).join(', or ') : 'this one operation'
  const rows: [string, preact.ComponentChildren][] = [
    ['website', f.website],
    ['asked by', `${f.requester.label} (${f.requester.kind})`],
    ['for', f.for],
    ['lasts', lasts],
    ['what', KIND_LABEL[f.kind]],
    [
      'files',
      <ul class="plain">
        {f.files.map((x) => (
          <li>
            {x.name} {x.size !== null ? <span class="muted">· {fmtBytes(x.size)}</span> : <span class="muted">· size unknown</span>}
          </li>
        ))}
      </ul>,
    ],
    ['saves to', f.destination],
    ['revision', String(f.revision)],
    ['requested', fmtStamp(Date.parse(f.issued_at))],
    ['expires', fmtStamp(Date.parse(f.expires_at))],
    ['request hash', <span class="mono break">{a.requestHash}</span>],
    ['bind code', <span class="mono bind">{a.bindCode}</span>],
  ]
  return (
    <div class="facts">
      {rows.map(([k, v]) => (
        <div class="row">
          <div>{k}</div>
          <div>{v}</div>
        </div>
      ))}
    </div>
  )
}

export function ApprovalDetail({ id }: { id: string }) {
  const a = useStore((s) => s.approvals.find((x) => x.id === id))
  const [seenHash, setSeenHash] = useState<string | null>(null)
  const [armed, setArmed] = useState(false)
  const paintedAt = useRef(0)
  const [code, setCode] = useState('')
  const [result, setResult] = useState<DecideResult | null>(null)
  const [changed, setChanged] = useState(false)

  // Arm after the facts are painted, and again after every change of the facts.
  useEffect(() => {
    if (!a) return undefined
    if (seenHash !== null && seenHash !== a.requestHash) {
      setChanged(true)
      setCode('')
    }
    setSeenHash(a.requestHash)
    setArmed(false)
    paintedAt.current = performance.now()
    const t = setTimeout(() => setArmed(true), ARMING_MS)
    return () => clearTimeout(t)
  }, [a?.requestHash])

  if (!a) return <Empty>No such request. <a href={href('approvals')}>All approvals</a></Empty>
  const pending = a.state === 'pending'
  const typed = a.facts.confirmation === 'typed_code'
  const codeOk = !typed || codeMatches(code, a.bindCode)
  const terminalOnly = a.facts.kind === 'cloud_delivery' || a.heldBy === 'zeusd'
  const act = (decision: 'approve' | 'deny', scope?: Scope) => {
    const r = decide(a.id, decision, scope, seenHash ?? '', performance.now() - paintedAt.current)
    setResult(r)
    if (r.ok) setChanged(false)
  }
  const approveScopes: Scope[] = a.facts.allowed_scopes.filter((s) => s !== 'never')
  return (
    <div class="page trust">
      <PageHead title={pending ? 'Decide this request' : 'Request'}>
        <a class="btn" href={href('approvals')}>
          All approvals
        </a>
      </PageHead>
      <p>
        <StateLabel state={a.state} /> {a.decided ? <span class="muted"> · {a.decided.decision === 'approve' ? 'approved' : 'denied'} {a.decided.scope ? `(${SCOPE_LASTS[a.decided.scope]})` : ''} {fmtStamp(a.decided.at)} via {a.decided.via}</span> : null}
      </p>
      {changed && pending ? <Notice kind="warn">The request changed (now revision {a.facts.revision}). Read the facts again; the buttons arm again and any typed code was cleared.</Notice> : null}
      <Facts a={a} />
      {a.agentSays ? (
        <div class="agentsays">
          <div class="label">{a.facts.requester.kind === 'agent' ? 'AI says' : 'Requester says'} · not a fact, not checked by Zeus</div>
          <div class="body">{a.agentSays}</div>
        </div>
      ) : null}
      {pending && terminalOnly ? (
        <div class="stack-tight">
          <Notice>
            {a.heldBy === 'zeusd'
              ? 'zeusd holds this request. It is decided only by a person on a zeusd trust surface: the interactive CLI or the extension popup. This page shows the facts and cannot decide.'
              : 'Cloud deliveries are decided only at a verified terminal, where you type the bind code. This screen shows the facts and cannot decide.'}
          </Notice>
          <CodeCopy text={`zeus approvals approve ${a.id}`} />
        </div>
      ) : null}
      {pending && !terminalOnly ? (
        <>
          {typed ? (
            <label class="field typed">
              Type the bind code to approve
              <input class="mono" value={code} autocomplete="off" spellcheck={false} aria-describedby="typedhelp" onInput={(e) => setCode((e.target as HTMLInputElement).value)} />
              <span id="typedhelp" class="muted small">
                Shown above as {a.bindCode}. Case does not matter; O reads as 0, I and L as 1.
              </span>
            </label>
          ) : null}
          <div class="decide-grid" role="group" aria-label="Decision">
            {(approveScopes.length ? approveScopes : [undefined]).map((s) => (
              <button type="button" class="btn decide" disabled={!armed || !codeOk} onClick={() => act('approve', s)}>
                {s ? SCOPE_BUTTON[s] : 'Approve'}
              </button>
            ))}
            <button type="button" class="btn decide" onClick={() => act('deny')}>
              Deny
            </button>
            {a.facts.allowed_scopes.includes('never') ? (
              <button type="button" class="btn decide" disabled={!armed} onClick={() => act('deny', 'never')}>
                {SCOPE_BUTTON.never}
              </button>
            ) : null}
          </div>
          <p class="muted small">
            {armed ? 'Armed.' : 'The approve buttons arm in a moment.'} Your decision binds to request hash {a.requestHash.slice(0, 16)}…; if the facts change first, it is refused and you read them again.
          </p>
        </>
      ) : null}
      {result && !result.ok ? <Notice kind="error">{result.reason}</Notice> : null}
      {result?.ok ? <Notice kind="ok">Decided. {a.facts.kind === 'session_grant' || a.facts.kind === 'handoff' ? 'The extension supplies the session for this site in memory only; Zeus never stores your cookies.' : ''}</Notice> : null}
      {a.jobIds.length ? (
        <p>
          <a href={href('downloads', { job: a.jobIds[0] })}>Show the download{a.jobIds.length > 1 ? 's' : ''} it covers</a>{' '}
          <Chip>{a.jobIds.length}</Chip>
        </p>
      ) : null}
      <Provenance>
        {a.heldBy === 'zeusd'
          ? 'zeusd computed this request hash (BLAKE3 over the JCS form of the facts) and accepts a decision only from a person on its own trust surfaces.'
          : NATIVE
            ? 'Held by this app: the hash is SHA-256 over the canonical facts (zeusd uses BLAKE3 over JCS), and the app checks the hash, the arming delay and the scope again before it records your decision. Nothing reaches zeusd until you approve.'
            : LIVE
            ? 'Held by the pre-alpha service: the hash is SHA-256 over the canonical facts (zeusd uses BLAKE3 over JCS), and the service checks the hash, the arming delay and the scope again before it records your decision.'
            : 'zeusd hashes the facts with BLAKE3 over their JCS form; this release computes a SHA-256 stand-in over the same canonical facts. In zeusd, decisions come from the terminal or the extension popup; here they change only sample data.'}
      </Provenance>
    </div>
  )
}
