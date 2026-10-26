// The site grabber: start page, depth, file types and include or exclude rules, then a preview of every page it would
// read and every file it would fetch, before anything is fetched. Running it is a workflow plan in zeusd terms, so the
// run waits for your approval with the bind code typed in (spec 07, workflow_plan).

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import { siteRequestRun, useStore } from '../../backend/store'
import type { SitePreview, SiteRequest } from '../../backend/sitegrab'
import { HOSTED, LIVE, NATIVE, SVC } from '../../backend/live'
import { call, svc } from '../../backend/remote'
import { previewSite } from '../../backend/sitegrab'
import { parseExtList } from '../../model/categories.ts'
import { fmtBytes } from '../../model/units.ts'
import { href, navigate } from '../../lib/router'
import { Dialog, Notice, NumberInput, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'

const TEMPLATES: { id: string; label: string; types: string; depth: number }[] = [
  { id: 'docs', label: 'Documents from a site', types: 'pdf epub docx xlsx pptx', depth: 2 },
  { id: 'media', label: 'Pictures and video', types: 'jpg png webp mp4 webm', depth: 2 },
  { id: 'archives', label: 'Archives and installers', types: 'zip 7z gz exe msi', depth: 1 },
  { id: 'custom', label: 'Custom settings', types: '', depth: 1 },
]

const STEPS = ['Start page', 'How deep', 'File types', 'Rules', 'Preview']

export function SiteGrabber({ onClose }: { onClose: () => void }) {
  const queues = useStore((s) => s.queues.filter((q) => q.kind === 'download'))
  const [step, setStep] = useState(0)
  const [name, setName] = useState('project_1')
  const [template, setTemplate] = useState('docs')
  const [startUrl, setStartUrl] = useState(NATIVE ? '' : HOSTED ? 'http://docs.example.org/' : 'https://docs.example.org/handbook/')
  const [delayMs, setDelayMs] = useState(1000)
  const walkId = useRef('')
  const [depth, setDepth] = useState(2)
  const [sameHost, setSameHost] = useState(true)
  const [types, setTypes] = useState('pdf epub docx xlsx pptx')
  const [include, setInclude] = useState('')
  const [exclude, setExclude] = useState('*/blog/*\n*draft*')
  const [maxFiles, setMaxFiles] = useState(50)
  const [maxMiB, setMaxMiB] = useState(2048)
  const startNow = useStore((s) => s.settings.downloads.startImmediately)
  const defaultQueue = useStore((s) => s.settings.scheduler.defaultQueue)
  const [queue, setQueue] = useState<string>(NATIVE && startNow ? 'now' : NATIVE ? defaultQueue : 'main')
  const [error, setError] = useState<string | null>(null)

  const req: SiteRequest = { startUrl, depth, sameHostOnly: sameHost, types: parseExtList(types), include: include.split('\n'), exclude: exclude.split('\n'), maxFiles, maxMiB, ...(NATIVE ? { delayMs } : {}) }
  const scripted = useMemo(() => (step === 4 && !LIVE ? previewSite(req) : null), [step, startUrl, depth, sameHost, types, include, exclude, maxFiles, maxMiB])
  // Live: the pre-alpha service walks the sample site for real (hosted). A local zeusd has no site walk in this client.
  const [walked, setWalked] = useState<SitePreview | null>(null)
  const [walking, setWalking] = useState(false)
  useEffect(() => {
    if (step !== 4 || !LIVE) return
    if (!SVC) {
      setWalked({ ok: false, error: 'Site walks run in the hosted pre-alpha service or in the desktop app. With a local zeusd behind a bridge, use zeus workflow draft and run in the CLI.', host: '', pages: [], files: [], skipped: [], totalBytes: 0 })
      return
    }
    let alive = true
    const id = `w${Date.now().toString(36)}${Math.random().toString(36).slice(2, 8)}`
    walkId.current = id
    setWalking(true)
    setWalked(null)
    svc
      .sitePreview(NATIVE ? { ...req, walkId: id } : req)
      .then((r) => alive && setWalked(r as SitePreview))
      .catch((e: unknown) => alive && setWalked({ ok: false, error: e instanceof Error ? e.message : 'The walk failed.', host: '', pages: [], files: [], skipped: [], totalBytes: 0 }))
      .finally(() => alive && setWalking(false))
    return () => {
      alive = false
      // Leaving the step, or the dialog, stops a walk that is still reading pages.
      if (NATIVE) void call('POST', '/native/v1/sitegrab/cancel', { walkId: id }).catch(() => undefined)
    }
  }, [step, startUrl, depth, sameHost, types, include, exclude, maxFiles, maxMiB, delayMs])
  const preview = LIVE ? walked : scripted
  const startOk = (() => {
    try {
      const u = new URL(startUrl)
      return (u.protocol === 'https:' || u.protocol === 'http:') && !u.username && !u.password
    } catch {
      return false
    }
  })()

  const run = () => {
    const p = siteRequestRun(req, name, queue === 'now' ? null : queue, LIVE && preview ? preview : undefined)
    if (typeof p === 'string') return setError(p)
    onClose()
    if (p.approvalId) navigate(href('approvals', p.approvalId))
  }

  return (
    <Dialog title={`Site grabber · ${STEPS[step]}`} onClose={onClose} wide>
      <ol class="steps" aria-label="Steps">
        {STEPS.map((s, i) => (
          <li class={i === step ? 'on' : i < step ? 'done' : ''}>
            <button type="button" disabled={i > step && !startOk} onClick={() => setStep(i)}>
              <span class="n">{i + 1}</span> {s}
            </button>
          </li>
        ))}
      </ol>
      {step === 0 ? (
        <div class="fields">
          <label class="field">
            Project name
            <input value={name} onInput={(e) => setName((e.target as HTMLInputElement).value)} />
          </label>
          <label class="field">
            Template
            <select
              value={template}
              onChange={(e) => {
                const t = TEMPLATES.find((x) => x.id === (e.target as HTMLSelectElement).value)
                setTemplate(t?.id ?? 'custom')
                if (t && t.id !== 'custom') {
                  setTypes(t.types)
                  setDepth(t.depth)
                }
              }}
            >
              {TEMPLATES.map((t) => (
                <option value={t.id}>{t.label}</option>
              ))}
            </select>
          </label>
          <label class="field wide">
            Start page
            <input class="mono" value={startUrl} placeholder={NATIVE ? 'https://example.org/downloads/' : undefined} onInput={(e) => setStartUrl((e.target as HTMLInputElement).value)} />
          </label>
          {!startOk && startUrl ? <p class="field-err wide">An http or https address without a user name or password.</p> : null}
          <Notice>Logged-in pages are not walked here. A page that needs your login goes through the Zeus extension, after you approve the site.{NATIVE ? ' The walk reads the site\'s robots.txt first and keeps to it, identifies itself as Zeus, and spaces its requests.' : ''}</Notice>
        </div>
      ) : null}
      {step === 1 ? (
        <div class="rows">
          <div class="row">
            <div class="text">
              <div class="title">Depth</div>
              <div class="desc">How many links away from the start page to read pages. 0 reads only the start page.</div>
            </div>
            <div class="control">
              <NumberInput label="Depth" value={depth} min={0} max={4} onChange={setDepth} unit="links" />
            </div>
          </div>
          <div class="row">
            <div class="text">
              <div class="title">Stay on this site</div>
              <div class="desc">{NATIVE ? 'Links to other hosts are counted and listed as skipped. Untick it to let the walk follow links to up to five hosts (each one\'s robots.txt and pace are kept).' : 'Links to other hosts are counted and listed as skipped, never followed.'}</div>
            </div>
            <div class="control">
              <Switch label="Stay on this site" checked={sameHost} onChange={setSameHost} />
            </div>
          </div>
          {NATIVE ? (
            <div class="row">
              <div class="text">
                <div class="title">Pause between requests</div>
                <div class="desc">To one host. At least 250 ms (four a second), and never faster than the site asks for in its robots.txt (Crawl-delay).</div>
              </div>
              <div class="control">
                <NumberInput label="Pause between requests" value={delayMs} min={250} max={10000} onChange={setDelayMs} unit="ms" />
              </div>
            </div>
          ) : null}
          <div class="row">
            <div class="text">
              <div class="title">Limits</div>
              <div class="desc">The run stops at whichever comes first. zeusd enforces page, file, byte and time budgets for a plan.</div>
            </div>
            <div class="control">
              <NumberInput label="Most files" value={maxFiles} min={1} max={500} onChange={setMaxFiles} unit="files" />
              <NumberInput label="Most MiB" value={maxMiB} min={1} max={102400} onChange={setMaxMiB} unit="MiB" />
            </div>
          </div>
        </div>
      ) : null}
      {step === 2 ? (
        <div class="stack-tight">
          <label class="field">
            File types to fetch
            <input class="mono" value={types} placeholder="pdf zip mp4" onInput={(e) => setTypes((e.target as HTMLInputElement).value)} />
          </label>
          <div class="chips">
            {['pdf', 'epub', 'docx', 'xlsx', 'zip', '7z', 'gz', 'mp4', 'mp3', 'jpg', 'png', 'exe', 'msi', 'csv'].map((e) => {
              const on = parseExtList(types).includes(e)
              return (
                <button type="button" class={`catchip ${on ? 'on' : ''}`} aria-pressed={on} onClick={() => setTypes(on ? parseExtList(types).filter((x) => x !== e).join(' ') : `${types} ${e}`.trim())}>
                  .{e}
                </button>
              )
            })}
          </div>
          <p class="muted small">Leave empty to take every file type. Pages themselves are read, never saved.</p>
        </div>
      ) : null}
      {step === 3 ? (
        <div class="fields">
          <label class="field">
            Include only paths matching <span class="muted">one per line, * and ? allowed</span>
            <textarea class="mono" rows={5} value={include} placeholder="*/2026/*" onInput={(e) => setInclude((e.target as HTMLTextAreaElement).value)} />
          </label>
          <label class="field">
            Exclude paths matching
            <textarea class="mono" rows={5} value={exclude} onInput={(e) => setExclude((e.target as HTMLTextAreaElement).value)} />
          </label>
        </div>
      ) : null}
      {step === 4 && walking ? <Notice>{NATIVE ? 'Walking the site, politely: robots.txt first, then one request at a time with a pause between them. A deep walk takes a minute or more.' : 'Walking the sample site…'}</Notice> : null}
      {step === 4 && preview ? (
        preview.ok ? (
          <div class="stack-tight">
            <div class="probe">
              <Icon name="eye" />
              <span>
                Would read <strong>{preview.pages.length} pages</strong> and fetch <strong>{preview.files.length} files</strong>, {fmtBytes(preview.totalBytes)} in all.
                {preview.unknownSizes ? ` ${preview.unknownSizes} file${preview.unknownSizes === 1 ? '' : 's'} did not give a size.` : ''}
              </span>
            </div>
            {NATIVE && preview.robots ? (
              <p class="muted small">
                {preview.robots.found ? `robots.txt read: ${preview.robots.rules} rule${preview.robots.rules === 1 ? '' : 's'} for Zeus${preview.robots.crawl_delay_ms ? `, a crawl delay of ${preview.robots.crawl_delay_ms / 1000} s kept` : ''}.` : 'The site has no robots.txt.'} {preview.requests} requests in {Math.round((preview.elapsedMs ?? 0) / 1000)} s.
              </p>
            ) : null}
            <div class="table-wrap short">
              <table>
                <thead>
                  <tr>
                    <th>File</th>
                    <th class="num">Size</th>
                    <th>Depth</th>
                    <th>Found on</th>
                  </tr>
                </thead>
                <tbody>
                  {preview.files.map((f) => (
                    <tr>
                      <td>{f.name}</td>
                      <td class="num">{fmtBytes(f.size)}</td>
                      <td>{f.depth}</td>
                      <td class="mono small">{f.from.replace(/^https?:\/\//, '')}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            {preview.skipped.length ? (
              <p class="muted small">
                Skipped: {preview.skipped.map((s) => `${s.count} ${s.reason}`).join(' · ')}
              </p>
            ) : null}
            <details>
              <summary>Pages it would read ({preview.pages.length})</summary>
              <ul class="plain mono small">
                {preview.pages.map((p) => (
                  <li>
                    {p.depth} · {p.url}
                  </li>
                ))}
              </ul>
            </details>
            <Notice>
              {NATIVE
                ? 'This app read these pages from your computer just now, keeping to the site\'s robots.txt. Nothing is downloaded yet. The run waits for your approval, held by this app, with the bind code typed in; the approved files then go to zeusd as one batch.'
                : LIVE
                ? 'The pre-alpha service read these pages from the sample site just now. The run waits for your approval with the bind code typed in; the approved files then go to zeusd as one reviewed batch.'
                : 'This preview is generated from the start address in this release; zeusd samples the real page and validates the plan before a run. The run waits for your approval with the bind code typed in.'}
            </Notice>
            {error ? <Notice kind="error">{error}</Notice> : null}
          </div>
        ) : (
          <Notice kind="error">{preview.error}</Notice>
        )
      ) : null}
      <div class="decide-row split">
        <button type="button" class="btn" onClick={() => (step ? setStep(step - 1) : onClose())}>
          {step ? 'Back' : 'Cancel'}
        </button>
        {step < 4 ? (
          <button type="button" class="btn primary" disabled={!startOk} onClick={() => setStep(step + 1)}>
            Next
          </button>
        ) : (
          <span class="inline">
            <select aria-label="Queue" value={queue} onChange={(e) => setQueue((e.target as HTMLSelectElement).value)}>
              <option value="now">Start when approved</option>
              {queues.map((q) => (
                <option value={q.id}>{q.name}</option>
              ))}
            </select>
            <button type="button" class="btn primary" disabled={!preview?.ok || !preview.files.length} onClick={run}>
              Request the run
            </button>
          </span>
        )}
      </div>
    </Dialog>
  )
}
