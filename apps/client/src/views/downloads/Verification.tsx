// What zeusd records about a finished file and what a person can do with it: checksums, the published hash it was
// compared with, why it is quarantined, and "Verify again", which has zeusd re-read the file and compare. Completed and
// verified against a published hash are separate facts: a file can be complete without anyone having a hash to check it
// against. In the desktop app these come from zeusd's own job record; nothing here is computed in the window.

import { useEffect, useState } from 'preact/hooks'

import { NATIVE } from '../../backend/live'
import { RemoteError, api, call, native } from '../../backend/remote'
import type { EngineInfo, JobDetail } from '../../backend/remote'
import type { Job } from '../../model/types.ts'
import { fmtBytes, fmtStamp } from '../../model/units.ts'
import { Chip, CodeCopy, CopyButton, Notice } from '../../ui/components'
import '../../styles/grabber.css'

/** The reasons zeusd gives for quarantine (spec 01 section 11.3), in a person's words. */
export const REASONS: Record<string, string> = {
  executable_type: 'It is a program, an installer or a script, or has the content of one.',
  mime_mismatch: 'Its content contradicts its name or the type the server declared (for example a document that is really a program).',
  html_disguised: 'It is a web page or script under a file name that says otherwise.',
  archive_limits: 'An archive that is unsafe to unpack (extreme compression, deep nesting, too many entries, or paths that escape the folder).',
  av_flagged: 'An antivirus hook reported it as not clean.',
  untrusted_origin: 'It came from a site you have not marked as trusted, and you asked Zeus to quarantine those.',
  session_derived: 'It was fetched with a browser login lent to Zeus, and you asked Zeus to quarantine those.',
}

interface Verified {
  sha256: string | null
  blake3: string | null
  size_bytes?: number
  detected_mime?: string | null
  matches_recorded: boolean
  matches_expected?: boolean | null
  quarantined: boolean
  quarantine_reasons?: string[]
}

type Detail = { job?: JobDetail & { result?: NonNullable<JobDetail['result']> } }

/** The same command with the app's environment, so a terminal talks to this app's engine. */
function terminalCommand(info: EngineInfo | null, args: string): string {
  if (!info) return `zeus ${args}`
  const win = info.bin.includes('\\')
  const exe = `${info.bin}${win ? '\\zeus.exe' : '/zeus'}`
  const env = Object.entries(info.env)
  return win ? `${env.map(([k, v]) => `$env:${k}='${v}'`).join('; ')}; & '${exe}' ${args}` : `${env.map(([k, v]) => `${k}='${v}'`).join(' ')} '${exe}' ${args}`
}

export function Verification({ job: j }: { job: Job }) {
  const [detail, setDetail] = useState<Detail | null>(null)
  const [info, setInfo] = useState<EngineInfo | null>(null)
  const [published, setPublished] = useState('')
  const [busy, setBusy] = useState(false)
  const [result, setResult] = useState<Verified | null>(null)
  const [err, setErr] = useState<string | null>(null)
  useEffect(() => {
    if (!NATIVE || !['completed', 'failed'].includes(j.state)) return
    let alive = true
    api.job(j.id).then(
      (d) => alive && setDetail(d as unknown as Detail),
      () => undefined,
    )
    return () => {
      alive = false
    }
  }, [j.id, j.state])
  useEffect(() => {
    if (j.quarantined && NATIVE) native.engine().then(setInfo, () => undefined)
  }, [j.quarantined])

  const d = detail?.job
  const sha = d?.result?.sha256 ?? j.sha256
  const blake = d?.result?.blake3 ?? j.blake3
  const expected = d?.spec?.expected_sha256 ?? j.expectedSha256 ?? null
  const reasons = d?.quarantine_reasons?.length ? d.quarantine_reasons : (j.quarantineReasons ?? []).filter((r) => r in REASONS)
  const quarantined = j.quarantined || !!d?.quarantined
  const shaOk = /^[0-9a-f]{64}$/.test(published.trim().toLowerCase())
  const mismatch = j.state === 'failed' && j.error?.code === 'integrity.hash_mismatch'

  const verify = (against?: string) => {
    setBusy(true)
    setErr(null)
    call<Verified>('POST', `/native/v1/jobs/${encodeURIComponent(j.id)}/verify`, against ? { expected_sha256: against.trim().toLowerCase() } : {})
      .then(setResult, (e: unknown) => setErr(e instanceof RemoteError ? (e.code === 'fs.dest_missing' ? 'The file is no longer at its place: it was moved or deleted.' : e.message) : 'zeusd did not answer.'))
      .finally(() => setBusy(false))
  }

  return (
    <section class="dsec">
      <h3>Checksum and verification</h3>
      {mismatch ? (
        <Notice kind="error">
          <div>The file did not match the published SHA-256 you gave. zeusd refused it: the job failed and no completed file exists under its name.</div>
          <div class="small">The bytes it received stay beside the destination as a .zpart partial file and are never renamed to the real name; zeusd keeps that partial for a failed job (a failed job cannot be cancelled). Check that the hash belongs to this exact file and version, or get the file from the publisher again.</div>
          <div class="code mono">{j.error?.code}</div>
        </Notice>
      ) : null}
      {j.state === 'completed' ? (
        <>
          <dl class="kvs">
            <dt>completed</dt>
            <dd>
              <Chip tone="ok">yes</Chip> <span class="muted small">every byte was committed, hashed and recorded{d?.result?.completed_at ? `, ${fmtStamp(Date.parse(d.result.completed_at))}` : ''}</span>
            </dd>
            <dt>SHA-256</dt>
            <dd>
              <span class="mono break small">{sha}</span>
            </dd>
            <dt>BLAKE3</dt>
            <dd>
              <span class="mono break small">{blake}</span>
            </dd>
            {d?.result ? (
              <>
                <dt>size</dt>
                <dd>
                  {fmtBytes(d.result.size_bytes)}
                  {d.result.detected_mime ? <span class="muted small"> · detected as {d.result.detected_mime}</span> : null}
                </dd>
              </>
            ) : null}
            <dt>verified against a published hash</dt>
            <dd>
              {expected ? (
                sha === expected ? (
                  <Chip tone="ok">yes: it matches the SHA-256 you gave</Chip>
                ) : (
                  <Chip tone="bad">no: it differs from the SHA-256 you gave</Chip>
                )
              ) : (
                <span class="muted">no: no published hash was given, so there is nothing to compare with</span>
              )}
            </dd>
            <dt>quarantine</dt>
            <dd>{quarantined ? <Chip tone="warn">quarantined</Chip> : 'no'}</dd>
          </dl>
          <div class="dactions">
            {sha ? <CopyButton text={sha} label="Copy SHA-256" /> : null}
            {blake ? <CopyButton text={blake} label="Copy BLAKE3" /> : null}
          </div>
          {NATIVE ? (
            <div class="verify-box">
              <div class="inline">
                <button type="button" class="btn small" disabled={busy} onClick={() => verify()}>
                  {busy ? 'Reading the file…' : 'Verify again'}
                </button>
                <span class="muted small">zeusd re-reads the file on disk and compares it with what it recorded.</span>
              </div>
              <label class="field">
                Compare with a published SHA-256
                <span class="inline">
                  <input class="mono" value={published} placeholder="64 hex digits from the publisher" onInput={(e) => setPublished((e.target as HTMLInputElement).value)} />
                  <button type="button" class="btn small" disabled={busy || !shaOk} onClick={() => verify(published)}>
                    Compare
                  </button>
                </span>
              </label>
              {result ? (
                <Notice kind={result.matches_recorded && result.matches_expected !== false ? 'ok' : 'error'}>
                  <div>{result.matches_recorded ? 'The file on disk still matches what zeusd recorded when it finished.' : 'The file on disk is different from what zeusd recorded: it was changed after the download.'}</div>
                  {result.matches_expected === true ? <div>It matches the published SHA-256 you gave.</div> : null}
                  {result.matches_expected === false ? <div>It does NOT match the published SHA-256 you gave.</div> : null}
                  {result.sha256 ? <div class="mono small break">{result.sha256}</div> : null}
                </Notice>
              ) : null}
              {err ? <Notice kind="error">{err}</Notice> : null}
            </div>
          ) : null}
          {quarantined ? (
            <div class="verify-box">
              <Notice kind="warn">
                <div>
                  <strong>This file is quarantined.</strong> Zeus never opens, runs or converts it. The window refuses to open it; Show in folder is still there, and what you do next is up to you.
                </div>
                {reasons.length ? (
                  <ul class="plain">
                    {reasons.map((r) => (
                      <li>
                        <span class="mono small">{r}</span>: {REASONS[r] ?? 'a quarantine rule of zeusd.'}
                      </li>
                    ))}
                  </ul>
                ) : null}
                <div class="small">
                  The flag cannot be cleared from here, or by any program: it is part of the record (spec 01, section 11.3). Moving the file out of its folder or deleting it through zeusd needs an approval that only you can give, at a terminal. For example:
                </div>
              </Notice>
              <CodeCopy text={`${terminalCommand(info, `files organize "${j.dir}" --rules rules.json --apply`)}\n${terminalCommand(info, 'approvals list')}`} />
            </div>
          ) : (
            <p class="muted small">Completed means the file was hashed and recorded. Verified against a published hash needs a hash from the publisher; the two stay separate facts.</p>
          )}
        </>
      ) : (
        <p class="muted small">Hashes are computed when every byte is committed. {expected ? 'An expected SHA-256 is set; a mismatch fails the job instead of completing it.' : ''}</p>
      )}
    </section>
  )
}
