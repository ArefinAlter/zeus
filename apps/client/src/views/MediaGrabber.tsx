import { useEffect, useState } from 'preact/hooks'

import { media } from '../backend/media'
import type { MediaInspection, MediaPlan, MediaTask } from '../backend/media'
import { fmtBytes } from '../model/units'
import { Chip, Notice } from '../ui/components'
import { MediaMux } from './MediaMux'

const active = (task: MediaTask) => task.state === 'downloading' || task.state === 'assembling' || task.state === 'converting'

/** Reviewed media acquisition. The native module keeps source URLs in memory;
 * no credentials or media addresses are retained in browser storage. */
export function MediaGrabber() {
  const [open, setOpen] = useState(false)
  const [url, setUrl] = useState('')
  const [origins, setOrigins] = useState('')
  const [inspection, setInspection] = useState<MediaInspection | null>(null)
  const [plan, setPlan] = useState<MediaPlan | null>(null)
  const [filename, setFilename] = useState('')
  const [root, setRoot] = useState(0)
  const [maxMiB, setMaxMiB] = useState(4096)
  const [connections, setConnections] = useState(4)
  const [resumeId, setResumeId] = useState('')
  const [tasks, setTasks] = useState<MediaTask[]>([])
  const [unreadable, setUnreadable] = useState(0)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const refresh = async () => {
    const result = await media.tasks()
    setTasks(result.items)
    setUnreadable(result.unreadable_checkpoints)
  }
  useEffect(() => {
    if (!open) return
    let stopped = false
    const poll = () => void media.tasks().then((result) => {
      if (!stopped) { setTasks(result.items); setUnreadable(result.unreadable_checkpoints) }
    }, (error: unknown) => { if (!stopped) setError(error instanceof Error ? error.message : 'The media task list is unavailable.') })
    poll()
    const timer = setInterval(poll, 1000)
    return () => { stopped = true; clearInterval(timer) }
  }, [open])
  const run = async (work: () => Promise<void>) => {
    setBusy(true); setError(''); setMessage('')
    try { await work() } catch (error) { setError(error instanceof Error ? error.message : 'The media request failed.') }
    finally { setBusy(false) }
  }
  const inspect = () => run(async () => {
    setPlan(null); setInspection(null)
    const result = await media.inspect(url.trim(), origins.split(/[\s,]+/).filter(Boolean))
    setInspection(result)
  })
  const select = (formatId: string) => run(async () => {
    if (!inspection) return
    const result = await media.select(inspection.inspection_id, formatId)
    if ('plan_id' in result) {
      setPlan(result); setFilename(result.filename); setRoot(0); setResumeId('')
    } else { setInspection(result); setPlan(null) }
  })
  const start = () => run(async () => {
    if (!plan) return
    const task = await media.start({
      plan_id: plan.plan_id, root_index: root, filename, max_bytes: Math.floor(maxMiB * 1024 * 1024),
      connections, idempotency_key: crypto.randomUUID(), ...(resumeId ? { resume_task_id: resumeId } : {}),
    })
    setMessage(`${resumeId ? 'Resumed' : 'Started'} ${task.filename}.`)
    setPlan(null); setInspection(null); setUrl(''); setResumeId('')
    await refresh()
  })
  const resumable = tasks.filter((task) => !active(task) && task.state !== 'complete' && task.fingerprint === plan?.fingerprint)
  return (
    <section class="media-grabber" aria-label="Media grabber">
      <button class="btn small" type="button" aria-expanded={open} onClick={() => setOpen(!open)}>
        {open ? 'Close media grabber' : 'Media grabber · HLS, DASH and direct media'}
      </button>
      {open ? <div class="media-panel">
        <h2>Save a media track</h2>
        <p class="muted small">Inspect a direct media URL, finite HLS/DASH manifest, or a page with media in its markup. Select a format and review it before saving.</p>
        <div class="media-fields">
          <label>Media or page URL<input type="url" value={url} autoComplete="off" spellcheck={false} onInput={(event) => setUrl(event.currentTarget.value)} placeholder="https://media.example.org/video/master.m3u8" /></label>
          <label>Additional media origins, if needed<input value={origins} autoComplete="off" spellcheck={false} onInput={(event) => setOrigins(event.currentTarget.value)} placeholder="https://cdn.example.org" /></label>
        </div>
        <button class="btn small" type="button" disabled={busy || !url.trim()} onClick={() => void inspect()}>{busy ? 'Working…' : 'Inspect media'}</button>
        {error ? <Notice kind="error">{error}</Notice> : null}
        {message ? <Notice kind="ok">{message}</Notice> : null}
        {inspection && !plan ? <div class="media-formats">
          <p class="small muted">Origins allowed for this inspection: {inspection.origins.join(', ')}</p>
          <ul>{inspection.formats.map((format) => <li key={format.format_id}>
            <div><strong>{format.label}</strong> <Chip>{format.container}</Chip>{format.bandwidth ? <span class="small muted"> {Math.round(format.bandwidth / 1000)} kb/s declared</span> : null}</div>
            {format.notices.map((notice) => <p class="small muted" key={notice}>{notice}</p>)}
            <button class="btn small" type="button" disabled={busy} onClick={() => void select(format.format_id)}>Select and review</button>
          </li>)}</ul>
        </div> : null}
        {plan ? <div class="media-review">
          <h3>Review {plan.format.label}</h3>
          <p>{plan.format.segment_count} resources{plan.format.duration_ms ? ` · ${Math.round(plan.format.duration_ms / 1000)} seconds` : ''} · {plan.format.container}</p>
          {plan.format.notices.map((notice) => <Notice key={notice}>{notice}</Notice>)}
          <p class="small muted">Origins: {plan.origins.join(', ')}. Verified segments are staged privately, then checked and assembled in order. Existing files are preserved.</p>
          <div class="media-fields">
            <label>Save in<select value={root} onChange={(event) => setRoot(Number(event.currentTarget.value))}>{plan.roots.map((path, index) => <option value={index} key={path}>{path}</option>)}</select></label>
            <label>File name<input value={filename} onInput={(event) => setFilename(event.currentTarget.value)} /></label>
            <label>Byte ceiling (MiB, including retries)<input type="number" min={1} max={102400} value={maxMiB} onInput={(event) => setMaxMiB(Number(event.currentTarget.value))} /></label>
            <label>Parallel resources<input type="number" min={1} max={8} value={connections} onInput={(event) => setConnections(Number(event.currentTarget.value))} /></label>
            {resumable.length ? <label>Acquisition<select value={resumeId} onChange={(event) => {
              const id = event.currentTarget.value; setResumeId(id)
              const task = tasks.find((task) => task.task_id === id)
              if (task) { setFilename(task.filename); setRoot(task.root_index) }
            }}><option value="">New acquisition</option>{resumable.map((task) => <option value={task.task_id} key={task.task_id}>Resume {task.filename} ({task.completed_segments}/{task.segment_count})</option>)}</select></label> : null}
          </div>
          <button class="btn primary small" type="button" disabled={busy || !filename || !Number.isFinite(maxMiB) || maxMiB < 1 || maxMiB > 102400 || !Number.isInteger(connections) || connections < 1 || connections > 8} onClick={() => void start()}>{resumeId ? 'Resume reviewed media' : 'Start reviewed media'}</button>
        </div> : null}
        {unreadable ? <Notice kind="warn">{unreadable} private media checkpoints could not be read. Their files were preserved for recovery.</Notice> : null}
        {tasks.length ? <div class="media-tasks"><h3>Media acquisitions</h3><ul>{tasks.map((task) => <li key={task.task_id}>
          <div><strong>{task.filename}</strong> <Chip tone={task.state === 'complete' ? 'ok' : task.state === 'failed' ? 'bad' : 'info'}>{task.state}</Chip></div>
          <p class="small muted">{task.completed_segments}/{task.segment_count} verified resources · {fmtBytes(task.verified_bytes)} retained · {fmtBytes(task.received_bytes)} received</p>
          {task.sha256 ? <p class="mono small">SHA-256 {task.sha256}</p> : null}
          {task.error_message ? <p class="small">{task.error_message}</p> : null}
          {!active(task) && task.state !== 'complete' && task.recovery_available ? <button class="btn small" type="button" disabled={busy} onClick={() => void run(async () => { await media.recover(task.task_id); await refresh() })}>Recover retained output without network</button> : null}
          <button class="btn small ghost" type="button" disabled={busy} onClick={() => void run(async () => {
            if (active(task)) await media.cancel(task.task_id)
            else await media.archive(task.task_id)
            await refresh()
          })}>{active(task) ? 'Stop and keep segments' : 'Archive task and clear segment cache'}</button>
        </li>)}</ul></div> : null}
        <MediaMux tasks={tasks} refresh={refresh} />
        <p class="small muted">Protected/encrypted media, live recording and script-only players are unavailable. Zeus never reads browser cookies. Interrupted acquisitions require a fresh inspection before an explicit resume; a complete set of retained segments can recover without network.</p>
      </div> : null}
    </section>
  )
}
