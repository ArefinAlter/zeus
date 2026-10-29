// IDM-Style Download Progress Popup: shows live multi-connection dynamic chunking,
// transfer rate, ETA, range status, connection table, and instant pause/resume controls.

import { useEffect, useState } from 'preact/hooks'
import { isActive, pause, resume, useStore } from '../../backend/store'
import { HOSTED, LIVE, NATIVE } from '../../backend/live'
import { native } from '../../backend/remote'
import { joinPath } from '../../lib/paths.ts'
import { href, navigate } from '../../lib/router'
import type { Job } from '../../model/types.ts'
import { fmtBytes, fmtEta, fmtPercent, fmtRate } from '../../model/units.ts'
import { Chip, CopyButton, Dialog, SegmentBar, StateLabel } from '../../ui/components'
import { Icon, categoryIcon } from '../../ui/icons'
import { jobEta, jobPercent, statusDetail } from './columns'

export function DownloadProgressDialog({
  jobId,
  onClose,
}: {
  jobId: string
  onClose: () => void
}) {
  const job = useStore((s) => s.jobs.find((j) => j.id === jobId))
  const now = useStore((s) => s.now)
  const [autoClose, setAutoClose] = useState(false)

  useEffect(() => {
    if (job?.state === 'completed' && autoClose) {
      const t = setTimeout(onClose, 800)
      return () => clearTimeout(t)
    }
    return undefined
  }, [job?.state, autoClose, onClose])

  if (!job) return null

  const p = jobPercent(job)
  const eta = jobEta(job)
  const active = isActive(job)
  const canPause = ['queued', 'probing', 'downloading', 'retry_scheduled', 'awaiting_session'].includes(job.state)
  const canResume = ['paused', 'failed', 'queued', 'retry_scheduled'].includes(job.state)
  const fullPath = joinPath(job.dir, job.name)

  const numConn = job.segments.length || job.connections || 8
  const connSpeed = job.rate > 0 && numConn > 0 ? job.rate / numConn : 0

  return (
    <Dialog
      title={`Download Status: ${job.name}`}
      onClose={onClose}
    >
      <div class="idm-popup">
        {/* Top Summary Banner */}
        <div class="idm-header">
          <Icon name={categoryIcon(job.category)} />
          <div class="idm-header-info">
            <div class="idm-title" title={job.name}>
              {job.name}
            </div>
            <div class="idm-meta">
              <StateLabel state={job.state} /> · {statusDetail(job, now)}
            </div>
          </div>
          <div class="idm-header-pct">{fmtPercent(p)}</div>
        </div>

        {/* Detailed Metrics Table */}
        <div class="idm-grid">
          <div class="idm-label">URL:</div>
          <div class="idm-val idm-url-row">
            <span class="mono small idm-url" title={job.url}>
              {job.url}
            </span>
            <CopyButton text={job.url} label="Copy address" />
          </div>

          <div class="idm-label">Save To:</div>
          <div class="idm-val mono small" title={fullPath}>
            {fullPath}
          </div>

          <div class="idm-label">File Size:</div>
          <div class="idm-val">
            <strong>{job.size !== null ? fmtBytes(job.size) : 'Unknown'}</strong>
            {job.size !== null ? ` (${job.size.toLocaleString()} bytes)` : ''}
          </div>

          <div class="idm-label">Downloaded:</div>
          <div class="idm-val">
            <strong>{fmtBytes(job.done)}</strong>
            <span class="muted small"> ({p !== null ? `${p.toFixed(1)}%` : 'streaming'})</span>
          </div>

          <div class="idm-label">Transfer Rate:</div>
          <div class="idm-val idm-rate">
            <Icon name="gauge" />
            <strong>{fmtRate(job.rate)}</strong>
          </div>

          <div class="idm-label">Time Left (ETA):</div>
          <div class="idm-val">
            <strong>{fmtEta(eta)}</strong>
          </div>

          <div class="idm-label">Range Support:</div>
          <div class="idm-val">
            <Chip tone={job.resumable === 'unsupported' ? 'bad' : 'ok'}>
              {job.resumable === 'unsupported' ? 'No (Single stream)' : 'Yes (Byte Ranges Supported)'}
            </Chip>
          </div>
        </div>

        {/* Dynamic Chunk Progress Bar */}
        <div class="idm-chunk-section">
          <div class="idm-chunk-header">
            <span>
              <strong>Dynamic Segment Allocation:</strong> {job.segments.length} parallel range streams
            </span>
            <span class="muted small">
              {job.segments.filter((s) => s.state === 'done').length} completed ·{' '}
              {job.segments.filter((s) => s.state === 'active').length} downloading
            </span>
          </div>
          <SegmentBar segments={job.segments} size={job.size} />
        </div>

        {/* Multi-connection Stream Table */}
        {job.segments.length > 0 ? (
          <div class="idm-connections-wrap">
            <table class="idm-table">
              <thead>
                <tr>
                  <th>Stream</th>
                  <th>Byte Range</th>
                  <th>Downloaded</th>
                  <th>Stream Speed</th>
                  <th>Status</th>
                </tr>
              </thead>
              <tbody>
                {job.segments.map((seg, i) => {
                  const span = seg.end - seg.start
                  const segPct = span > 0 ? (seg.done / span) * 100 : 0
                  return (
                    <tr key={i}>
                      <td>Connection #{i + 1}</td>
                      <td class="mono small">
                        {fmtBytes(seg.start)} – {fmtBytes(seg.end)}
                      </td>
                      <td class="mono small">
                        {fmtBytes(seg.done)} ({segPct.toFixed(0)}%)
                      </td>
                      <td class="mono small">
                        {seg.state === 'active' ? fmtRate(connSpeed) : '—'}
                      </td>
                      <td>
                        <Chip tone={seg.state === 'done' ? 'ok' : seg.state === 'active' ? 'ok' : 'info'}>
                          {seg.state === 'done' ? 'Complete' : seg.state === 'active' ? 'Active' : 'Pending'}
                        </Chip>
                      </td>
                    </tr>
                  )
                })}
              </tbody>
            </table>
          </div>
        ) : null}

        {/* Footer Actions */}
        <div class="idm-footer">
          <label class="check idm-autoclose">
            <input
              type="checkbox"
              checked={autoClose}
              onChange={(e) => setAutoClose((e.target as HTMLInputElement).checked)}
            />
            Close this dialog when download finishes
          </label>

          <div class="inline gap">
            {canPause ? (
              <button
                type="button"
                class="btn"
                onClick={() => pause([job.id])}
              >
                <Icon name="pause" /> Pause
              </button>
            ) : null}

            {canResume ? (
              <button
                type="button"
                class="btn primary"
                onClick={() => resume([job.id])}
              >
                <Icon name="play" /> {job.state === 'failed' ? 'Retry' : 'Resume'}
              </button>
            ) : null}

            {job.state === 'completed' ? (
              <button
                type="button"
                class="btn primary"
                onClick={() => {
                  if (NATIVE) {
                    native.open(joinPath(job.dir, job.name), 'open').catch(() => undefined)
                  }
                }}
              >
                <Icon name="external" /> Open File
              </button>
            ) : null}

            <button
              type="button"
              class="btn"
              onClick={() => {
                if (NATIVE) {
                  native.open(joinPath(job.dir, job.name), 'reveal').catch(() => native.open(job.dir, 'open').catch(() => undefined))
                } else {
                  onClose()
                  navigate(href('files', { path: job.dir }))
                }
              }}
            >
              <Icon name="folderopen" /> Open Folder
            </button>

            <button type="button" class="btn ghost" onClick={onClose}>
              Hide
            </button>
          </div>
        </div>
      </div>
    </Dialog>
  )
}
