import { useEffect, useState } from 'preact/hooks'

import { media } from '../backend/media'
import type { MediaHelper, MediaMuxPlan, MediaTask } from '../backend/media'
import { fmtBytes } from '../model/units'
import { Notice } from '../ui/components'

/** An optional stream-copy review. Executable trust is operator-owned; the
 * webview can only request a fixed profile over completed media identities. */
export function MediaMux({ tasks, refresh }: { tasks: MediaTask[]; refresh: () => Promise<void> }) {
  const [helper, setHelper] = useState<MediaHelper | null>(null)
  const [video, setVideo] = useState('')
  const [audio, setAudio] = useState('')
  const [container, setContainer] = useState<'mp4' | 'mkv'>('mp4')
  const [plan, setPlan] = useState<MediaMuxPlan | null>(null)
  const [filename, setFilename] = useState('')
  const [root, setRoot] = useState(0)
  const [maxMiB, setMaxMiB] = useState(4096)
  const [seconds, setSeconds] = useState(300)
  const [busy, setBusy] = useState(false)
  const [error, setError] = useState('')
  const [message, setMessage] = useState('')
  const complete = tasks.filter((task) => task.state === 'complete')
  useEffect(() => { void media.helper().then(setHelper, () => setHelper({ configured: false, message: 'The local helper status is unavailable.' })) }, [])
  const run = async (work: () => Promise<void>) => {
    setBusy(true); setError(''); setMessage('')
    try { await work() } catch (error) { setError(error instanceof Error ? error.message : 'The mux operation failed.') }
    finally { setBusy(false) }
  }
  return <section class="media-review" aria-label="Local media mux">
    <h3>Combine or repackage completed media</h3>
    <p class="small muted">Copy existing video/audio streams into MP4 or MKV without re-encoding. Originals are preserved. This uses an optional local FFmpeg helper.</p>
    {!helper?.configured ? <Notice>{helper?.message ?? 'Reading the operator-owned helper configuration…'} <a href="https://github.com/ArefinAlter/zeus/blob/main/docs/alpha-0.1/MEDIA.md" target="_blank" rel="noreferrer">Helper setup and limits</a></Notice> : <>
      <div class="media-fields">
        <label>Media or video track<select value={video} onChange={(event) => { setVideo(event.currentTarget.value); if (audio === event.currentTarget.value) setAudio(''); setPlan(null) }}><option value="">Choose completed media</option>{complete.map((task) => <option key={task.task_id} value={task.task_id}>{task.filename}</option>)}</select></label>
        <label>Separate audio track, optional<select value={audio} onChange={(event) => { setAudio(event.currentTarget.value); setPlan(null) }}><option value="">Use audio in the first file</option>{complete.filter((task) => task.task_id !== video).map((task) => <option key={task.task_id} value={task.task_id}>{task.filename}</option>)}</select></label>
        <label>Output container<select value={container} onChange={(event) => { setContainer(event.currentTarget.value as 'mp4' | 'mkv'); setPlan(null) }}><option value="mp4">MP4 (fragmented)</option><option value="mkv">MKV</option></select></label>
      </div>
      <button class="btn small" type="button" disabled={busy || !video || audio === video} onClick={() => void run(async () => {
        const review = await media.muxPreview(audio ? [video, audio] : [video], container)
        setPlan(review); setFilename(review.filename); setRoot(0)
      })}>Review stream-copy operation</button>
      {plan ? <div class="media-review">
        <p>{plan.inputs.map((input) => input.filename).join(' + ')} · {fmtBytes(plan.input_bytes)} of input copied to private staging.</p>
        {plan.notices.map((notice) => <p class="small muted" key={notice}>{notice}</p>)}
        <p class="mono small">Helper SHA-256 {plan.helper.sha256}</p>
        <div class="media-fields">
          <label>Save in<select value={root} onChange={(event) => setRoot(Number(event.currentTarget.value))}>{plan.roots.map((path, index) => <option value={index} key={path}>{path}</option>)}</select></label>
          <label>Output filename<input value={filename} onInput={(event) => setFilename(event.currentTarget.value)} /></label>
          <label>Total helper output ceiling (MiB)<input type="number" min={1} max={102400} value={maxMiB} onInput={(event) => setMaxMiB(Number(event.currentTarget.value))} /></label>
          <label>Time limit (seconds)<input type="number" min={1} max={3600} value={seconds} onInput={(event) => setSeconds(Number(event.currentTarget.value))} /></label>
        </div>
        <button class="btn primary small" type="button" disabled={busy || !filename || !Number.isFinite(maxMiB) || maxMiB < 1 || maxMiB > 102400 || !Number.isFinite(seconds) || seconds < 1 || seconds > 3600} onClick={() => void run(async () => {
          const task = await media.muxStart({ plan_id: plan.plan_id, root_index: root, filename, max_bytes: Math.floor(maxMiB * 1024 * 1024), timeout_ms: Math.floor(seconds * 1000), idempotency_key: crypto.randomUUID() })
          setMessage(`Started ${task.filename}.`); setPlan(null); await refresh()
        })}>Start reviewed stream copy</button>
      </div> : null}
    </>}
    {error ? <Notice kind="error">{error}</Notice> : null}
    {message ? <Notice kind="ok">{message}</Notice> : null}
  </section>
}
