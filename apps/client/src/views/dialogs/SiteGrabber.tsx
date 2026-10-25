// The site grabber: start page, depth, file types and include or exclude rules, then a preview of every page it would
// read and every file it would fetch, before anything is fetched. Running it is a workflow plan in zeusd terms, so the
// run waits for your approval with the bind code typed in (spec 07, workflow_plan).

import { useMemo, useState } from 'preact/hooks'

import { siteRequestRun, useStore } from '../../backend/store'
import type { SiteRequest } from '../../backend/sitegrab'
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
  const [startUrl, setStartUrl] = useState('https://docs.example.org/handbook/')
  const [depth, setDepth] = useState(2)
  const [sameHost, setSameHost] = useState(true)
  const [types, setTypes] = useState('pdf epub docx xlsx pptx')
  const [include, setInclude] = useState('')
  const [exclude, setExclude] = useState('*/blog/*\n*draft*')
  const [maxFiles, setMaxFiles] = useState(50)
  const [maxMiB, setMaxMiB] = useState(2048)
  const [queue, setQueue] = useState<string>('main')
  const [error, setError] = useState<string | null>(null)

  const req: SiteRequest = { startUrl, depth, sameHostOnly: sameHost, types: parseExtList(types), include: include.split('\n'), exclude: exclude.split('\n'), maxFiles, maxMiB }
  const preview = useMemo(() => (step === 4 ? previewSite(req) : null), [step, startUrl, depth, sameHost, types, include, exclude, maxFiles, maxMiB])
  const startOk = (() => {
    try {
      const u = new URL(startUrl)
      return (u.protocol === 'https:' || u.protocol === 'http:') && !u.username && !u.password
    } catch {
      return false
    }
  })()

  const run = () => {
    const p = siteRequestRun(req, name, queue === 'now' ? null : queue)
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
            <input class="mono" value={startUrl} onInput={(e) => setStartUrl((e.target as HTMLInputElement).value)} />
          </label>
          {!startOk && startUrl ? <p class="field-err wide">An http or https address without a user name or password.</p> : null}
          <Notice>Logged-in pages are not walked here. A page that needs your login goes through the Zeus extension, after you approve the site.</Notice>
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
              <div class="desc">Links to other hosts are counted and listed as skipped, never followed.</div>
            </div>
            <div class="control">
              <Switch label="Stay on this site" checked={sameHost} onChange={setSameHost} />
            </div>
          </div>
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
      {step === 4 && preview ? (
        preview.ok ? (
          <div class="stack-tight">
            <div class="probe">
              <Icon name="eye" />
              <span>
                Would read <strong>{preview.pages.length} pages</strong> and fetch <strong>{preview.files.length} files</strong>, {fmtBytes(preview.totalBytes)} in all.
              </span>
            </div>
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
            <Notice>This preview is generated from the start address in this release; zeusd samples the real page and validates the plan before a run. The run waits for your approval with the bind code typed in.</Notice>
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
