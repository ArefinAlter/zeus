// The properties panel of one download: progress per byte range, the facts zeusd records (address without its query
// string, requester, validators and resumability), checksum and verification as separate facts, the approval a job
// waits on, the error with its next step, and the job's history.

import { useState } from 'preact/hooks'

import { getState, isActive, moveJob, pause, queueName, redownload, refreshAddress, renameJob, resume, useStore } from '../../backend/store'
import { KIND_LABEL } from '../../model/approvals.ts'
import { categoryLabel } from '../../model/categories.ts'
import { nameProblem } from '../../model/links.ts'
import type { Job } from '../../model/types.ts'
import { fmtBytes, fmtEta, fmtPercent, fmtRate, fmtStamp, fmtWhen } from '../../model/units.ts'
import { href } from '../../lib/router'
import { Chip, CopyButton, Dialog, Notice, SegmentBar, StateLabel } from '../../ui/components'
import { Icon, categoryIcon } from '../../ui/icons'
import { jobEta, jobPercent, statusDetail } from './columns'

function Fact({ k, children }: { k: string; children: preact.ComponentChildren }) {
  return (
    <>
      <dt>{k}</dt>
      <dd>{children}</dd>
    </>
  )
}

export function Properties({ job: j, onClose, onMove }: { job: Job; onClose: () => void; onMove: () => void }) {
  const now = useStore((s) => s.now)
  const approval = useStore((s) => (j.approvalId ? s.approvals.find((a) => a.id === j.approvalId) : undefined))
  const [fresh, setFresh] = useState('')
  const [err, setErr] = useState<string | null>(null)
  const p = jobPercent(j)
  return (
    <aside class="drawer" aria-label={`Properties of ${j.name}`}>
      <div class="drawer-head">
        <Icon name={categoryIcon(j.category)} />
        <div class="dtitle">
          <div class="name" title={j.name}>
            {j.name}
          </div>
          <div class="muted small">
            <StateLabel state={j.state} /> {statusDetail(j, now)}
          </div>
        </div>
        <button type="button" class="icon-btn small" aria-label="Close properties" onClick={onClose}>
          <Icon name="x" />
        </button>
      </div>
      <div class="drawer-body">
        <section class="dsec">
          <div class="bigprogress">
            <span class="pct">{fmtPercent(p)}</span>
            <span class="muted small">
              {fmtBytes(j.state === 'verifying' ? (j.hashed ?? 0) : j.done)} of {j.size === null ? 'unknown size' : fmtBytes(j.size)}
              {j.state === 'downloading' ? ` · ${fmtRate(j.rate)} · ${fmtEta(jobEta(j))} left` : j.state === 'verifying' ? ' hashed' : ''}
            </span>
          </div>
          <h3>Byte ranges</h3>
          <SegmentBar segments={j.segments} size={j.size} />
          <div class="muted small">
            {j.segments.length} ranges, {j.segments.filter((s) => s.state === 'done').length} done, {j.segments.filter((s) => s.state === 'active').length} active · up to {j.connections} connections · range support {j.resumable}
          </div>
          <div class="dactions">
            {['queued', 'probing', 'downloading', 'retry_scheduled'].includes(j.state) ? (
              <button type="button" class="btn small" onClick={() => pause([j.id])}>
                <Icon name="pause" /> Pause
              </button>
            ) : null}
            {['paused', 'failed', 'queued', 'retry_scheduled'].includes(j.state) ? (
              <button type="button" class="btn small" onClick={() => resume([j.id])}>
                <Icon name="play" /> {j.state === 'failed' ? 'Retry' : j.state === 'queued' ? 'Start now' : j.state === 'retry_scheduled' ? 'Retry now' : 'Resume'}
              </button>
            ) : null}
            {j.state === 'completed' ? (
              <button type="button" class="btn small" onClick={() => redownload(j.id)}>
                <Icon name="redo" /> Download again
              </button>
            ) : null}
            <button type="button" class="btn small" disabled={isActive(j)} onClick={onMove}>
              <Icon name="rename" /> Move or rename
            </button>
            <a class="btn small" href={href('files', { path: j.dir })}>
              <Icon name="folderopen" /> Folder
            </a>
          </div>
        </section>

        {j.error ? (
          <section class="dsec">
            <h3>What happened</h3>
            <Notice kind={j.state === 'failed' ? 'error' : 'warn'}>
              <div>{j.error.message}</div>
              <div class="small">{j.error.next}</div>
              <div class="code mono">{j.error.code}</div>
            </Notice>
            {j.error.code === 'http.url_expired' ? (
              <div class="stack-tight">
                <button type="button" class="btn small primary" onClick={() => setErr(refreshAddress(j.id))}>
                  <Icon name="refresh" /> Ask the browser for a fresh address and retry
                </button>
                <label class="field">
                  Or paste the new address
                  <span class="inline">
                    <input value={fresh} placeholder="https://…" onInput={(e) => setFresh((e.target as HTMLInputElement).value)} />
                    <button type="button" class="btn small" disabled={!fresh.trim()} onClick={() => setErr(refreshAddress(j.id, fresh.trim()))}>
                      Use
                    </button>
                  </span>
                </label>
                {err ? <Notice kind="error">{err}</Notice> : null}
              </div>
            ) : null}
          </section>
        ) : null}

        {approval ? (
          <section class="dsec">
            <h3>Approval</h3>
            <dl class="kvs">
              <Fact k="kind">{KIND_LABEL[approval.facts.kind]}</Fact>
              <Fact k="state">
                <StateLabel state={approval.state} />
              </Fact>
              <Fact k="bind code">
                <span class="mono">{approval.bindCode}</span>
              </Fact>
              <Fact k="revision">{approval.facts.revision}</Fact>
            </dl>
            {approval.state === 'pending' ? (
              <a class="btn small" href={href('approvals', approval.id)}>
                <Icon name="seal" /> Review the request
              </a>
            ) : null}
          </section>
        ) : null}

        <section class="dsec">
          <h3>Facts</h3>
          <dl class="kvs">
            <Fact k="address">
              <span class="mono break">{j.url}</span>
              {j.queryWithheld ? <span class="muted small"> · query string withheld</span> : null}
            </Fact>
            <Fact k="host">{j.host}</Fact>
            <Fact k="asked by">{j.source.label}</Fact>
            <Fact k="added">{fmtStamp(j.added)}</Fact>
            {j.finished ? <Fact k="finished">{fmtStamp(j.finished)}</Fact> : null}
            <Fact k="folder">
              <span class="mono break">{j.dir}</span>
            </Fact>
            <Fact k="category">{categoryLabel(j.category)}</Fact>
            <Fact k="queue">{queueName(j.queue)}</Fact>
            <Fact k="priority">{j.priority}</Fact>
            {j.grabberProject ? <Fact k="project">{j.grabberProject}</Fact> : null}
            {j.sessionOrigin ? <Fact k="needs login">{j.sessionOrigin}</Fact> : null}
            <Fact k="job id">
              <span class="mono small">{j.id}</span>
            </Fact>
          </dl>
        </section>

        <section class="dsec">
          <h3>Checksum and verification</h3>
          {j.state === 'completed' ? (
            <>
              <dl class="kvs">
                <Fact k="SHA-256">
                  <span class="mono break small">{j.sha256}</span>
                </Fact>
                <Fact k="BLAKE3">
                  <span class="mono break small">{j.blake3}</span>
                </Fact>
                <Fact k="expected">{j.expectedSha256 ? (j.matchedExpected ? <Chip tone="ok">matches the published SHA-256</Chip> : <Chip tone="bad">does not match</Chip>) : <span class="muted">no published hash to compare with</span>}</Fact>
                <Fact k="quarantine">{j.quarantined ? <Chip tone="warn">quarantined: {(j.quarantineReasons ?? []).join(', ')}</Chip> : 'no'}</Fact>
              </dl>
              {j.sha256 ? <CopyButton text={j.sha256} label="Copy SHA-256" /> : null}
              {j.quarantined ? <p class="muted small">Zeus never opens, runs or converts a quarantined file. Moving it out of its folder or deleting it needs an approval. The operating system's download mark is applied to every file regardless.</p> : <p class="muted small">Completed means the file was hashed and recorded. Verified against its source needs a published hash; the two stay separate facts.</p>}
            </>
          ) : (
            <p class="muted small">Hashes are computed when every byte is committed. {j.expectedSha256 ? 'An expected SHA-256 is set; a mismatch fails the job instead of completing it.' : ''}</p>
          )}
        </section>

        <section class="dsec">
          <h3>History</h3>
          <ol class="events">
            {[...j.events].reverse().map((e) => (
              <li>
                <span class="muted small mono">{fmtWhen(e.t, now)}</span>
                <span>{e.text}</span>
              </li>
            ))}
          </ol>
        </section>
      </div>
    </aside>
  )
}

export function MoveRename({ id, onClose }: { id: string; onClose: () => void }) {
  const j = getState().jobs.find((x) => x.id === id)
  const roots = useStore((s) => s.settings.saveTo.writeRoots)
  const [name, setName] = useState(j?.name ?? '')
  const [dir, setDir] = useState(j?.dir ?? '')
  const [error, setError] = useState<string | null>(null)
  if (!j) return null
  const problem = nameProblem(name)
  const save = () => {
    let e: string | null = null
    if (name !== j.name) e = renameJob(j.id, name)
    if (!e && dir !== j.dir) e = moveJob(j.id, dir)
    if (e) setError(e)
    else onClose()
  }
  return (
    <Dialog title="Move or rename" onClose={onClose}>
      <label class="field">
        Name
        <input value={name} onInput={(e) => setName((e.target as HTMLInputElement).value)} />
      </label>
      {problem ? <p class="field-err">{problem}</p> : null}
      <label class="field">
        Folder
        <input class="mono" value={dir} onInput={(e) => setDir((e.target as HTMLInputElement).value)} />
      </label>
      <p class="muted small">Zeus writes only inside its download folders: {roots.join(', ')}. A name that exists already gets " (1)" instead of replacing a file.</p>
      {j.quarantined ? <Notice kind="warn">This file is quarantined. In zeusd, moving it out of its folder needs an approval.</Notice> : null}
      {error ? <Notice kind="error">{error}</Notice> : null}
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        <button type="button" class="btn primary" disabled={!!problem} onClick={save}>
          Save
        </button>
      </div>
    </Dialog>
  )
}
