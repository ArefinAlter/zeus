// Queues and the scheduler, and the speed limiter. A queue has its own start and stop times, days, how many files run
// at once, retries, a rate limit and what happens when it finishes; a synchronization queue checks finished files for
// newer versions on a period. zeusd does not schedule queues yet (IMPLEMENTATION_STATUS), so the page says so; the
// rules it shows are tested in model/schedule.ts and followed by this release's backend.

import { useState } from 'preact/hooks'

import { addQueue, deleteQueue, getState, moveInQueue, queueName, setLimiter, setQueue, startQueue, stopQueue, updateQueue, useStore } from '../backend/store'
import { describeSchedule, limiterActive, minutesUntil, nextStart, parseHm, stopAfter } from '../model/schedule.ts'
import type { OnFinish, Queue, Weekday } from '../model/types.ts'
import { fmtBytes, fmtEta, fmtWhen } from '../model/units.ts'
import { href, navigate } from '../lib/router'
import { Chip, Empty, NumberInput, PageHead, Provenance, SettingRow, SettingsGroup, StateLabel, Switch, Tabs } from '../ui/components'
import { Icon } from '../ui/icons'

const DAYS: [Weekday, string][] = [
  [1, 'Mon'],
  [2, 'Tue'],
  [3, 'Wed'],
  [4, 'Thu'],
  [5, 'Fri'],
  [6, 'Sat'],
  [0, 'Sun'],
]

const FINISH: [OnFinish, string][] = [
  ['nothing', 'Do nothing'],
  ['notify', 'Show a notification'],
  ['stop_limiter', 'Turn the speed limiter off'],
  ['close_app', 'Close the Zeus window'],
  ['sleep', 'Put the computer to sleep'],
]

export function DayPicker({ days, onChange, label }: { days: Weekday[]; onChange: (d: Weekday[]) => void; label: string }) {
  return (
    <div class="daypick" role="group" aria-label={label}>
      {DAYS.map(([d, n]) => (
        <button type="button" aria-pressed={days.includes(d)} class={days.includes(d) ? 'on' : ''} onClick={() => onChange(days.includes(d) ? days.filter((x) => x !== d) : [...days, d])}>
          {n}
        </button>
      ))}
    </div>
  )
}

function TimeInput({ value, onChange, label }: { value: string; onChange: (v: string) => void; label: string }) {
  const [text, setText] = useState(value)
  const bad = parseHm(text) === null
  return (
    <input
      class={`mono time ${bad ? 'bad' : ''}`}
      aria-label={label}
      value={text}
      size={5}
      onInput={(e) => setText((e.target as HTMLInputElement).value)}
      onBlur={() => {
        if (!bad) onChange(text.length === 4 ? `0${text}` : text)
        else setText(value)
      }}
    />
  )
}

function ScheduleTab({ q, now }: { q: Queue; now: number }) {
  const s = q.schedule
  const set = (patch: Partial<Queue['schedule']>) => updateQueue(q.id, { schedule: { ...s, ...patch } })
  const next = nextStart(s, now)
  return (
    <>
      <SettingsGroup title="Start">
        <SettingRow title="Start at a time" desc={next ? `Next start ${fmtWhen(next, now)}, in ${minutesUntil(next, now)} min.` : 'This queue starts only when you press Start.'}>
          <Switch label="Start at a time" checked={s.startEnabled} onChange={(v) => set({ startEnabled: v })} />
          <TimeInput label="Start time" value={s.startTime} onChange={(v) => set({ startTime: v })} />
        </SettingRow>
        <SettingRow title="Repeat" desc="Once on a date, or on the days you pick, in local time.">
          <select aria-label="Repeat" value={s.repeat} onChange={(e) => set({ repeat: (e.target as HTMLSelectElement).value as 'once' | 'days' })}>
            <option value="days">On these days</option>
            <option value="once">Once</option>
          </select>
          {s.repeat === 'once' ? <input type="date" aria-label="Date" value={s.startDate} onInput={(e) => set({ startDate: (e.target as HTMLInputElement).value })} /> : null}
        </SettingRow>
        {s.repeat === 'days' ? (
          <SettingRow title="Days" desc={describeSchedule(s)}>
            <DayPicker label="Days" days={s.days} onChange={(d) => set({ days: d })} />
          </SettingRow>
        ) : null}
      </SettingsGroup>
      <SettingsGroup title="Stop" note="At the stop time active files go back to waiting with their saved bytes. A global stop is separate from a schedule, and a schedule never releases it.">
        <SettingRow title="Stop at a time" desc={s.stopEnabled && q.running && q.lastStarted ? `Stops ${fmtWhen(stopAfter(s, q.lastStarted) ?? undefined, now)}.` : 'A stop time earlier than the start runs past midnight.'}>
          <Switch label="Stop at a time" checked={s.stopEnabled} onChange={(v) => set({ stopEnabled: v })} />
          <TimeInput label="Stop time" value={s.stopTime} onChange={(v) => set({ stopTime: v })} />
        </SettingRow>
        <SettingRow title="When it finishes" desc="After the last file of the queue is done.">
          <select aria-label="When it finishes" value={q.onFinish} onChange={(e) => updateQueue(q.id, { onFinish: (e.target as HTMLSelectElement).value as OnFinish })}>
            {FINISH.map(([id, label]) => (
              <option value={id}>{label}</option>
            ))}
          </select>
        </SettingRow>
        {q.onFinish === 'close_app' || q.onFinish === 'sleep' ? (
          <div class="row">
            <div class="text">
              <div class="desc">This release shows a notification instead: closing the window and sleeping the computer need the desktop app on a real zeusd.</div>
            </div>
          </div>
        ) : null}
      </SettingsGroup>
    </>
  )
}

function FilesTab({ q }: { q: Queue }) {
  const jobs = useStore((s) => s.jobs)
  const order = [...q.order, ...jobs.filter((j) => j.queue === q.id && !q.order.includes(j.id)).map((j) => j.id)]
  const rows = order.map((id) => jobs.find((j) => j.id === id)).filter((j): j is NonNullable<typeof j> => !!j && j.queue === q.id)
  return (
    <section class="sgroup">
      <div class="sgroup-head">
        <h2 class="sgroup-title">Files in the queue</h2>
        <div class="sgroup-actions">
          <span class="muted small">Download</span>
          <NumberInput label="Files at the same time" value={q.maxConcurrent} min={1} max={16} onChange={(v) => updateQueue(q.id, { maxConcurrent: v })} unit="at the same time" />
        </div>
      </div>
      {rows.length ? (
        <div class="table-wrap">
          <table>
            <thead>
              <tr>
                <th>#</th>
                <th>File</th>
                <th class="num">Size</th>
                <th>Status</th>
                <th class="num">Time left</th>
                <th />
              </tr>
            </thead>
            <tbody>
              {rows.map((j, i) => (
                <tr>
                  <td class="mono">{i + 1}</td>
                  <td>
                    <a href={href('downloads', { job: j.id })}>{j.name}</a>
                  </td>
                  <td class="num">{fmtBytes(j.size)}</td>
                  <td>
                    <StateLabel state={j.state} />
                  </td>
                  <td class="num">{j.state === 'downloading' && j.rate ? fmtEta(((j.size ?? 0) - j.done) / j.rate) : '–'}</td>
                  <td class="actions-cell">
                    <button type="button" class="icon-btn small" aria-label="Move up" disabled={i === 0} onClick={() => moveInQueue(q.id, j.id, -1)}>
                      <Icon name="up" />
                    </button>
                    <button type="button" class="icon-btn small" aria-label="Move down" disabled={i === rows.length - 1} onClick={() => moveInQueue(q.id, j.id, 1)}>
                      <Icon name="down" />
                    </button>
                    <button type="button" class="icon-btn small" aria-label="Take out of the queue" title="Take out of the queue" onClick={() => setQueue([j.id], null)}>
                      <Icon name="x" />
                    </button>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      ) : (
        <Empty>No files in this queue. Use Add to queue in a download's menu, or pick this queue in the link grabber.</Empty>
      )}
    </section>
  )
}

function LimitsTab({ q }: { q: Queue }) {
  return (
    <SettingsGroup title="Limits">
      <SettingRow title="Files at the same time" desc="Separate from connections per file, which Settings, Downloads sets.">
        <NumberInput label="Files at the same time" value={q.maxConcurrent} min={1} max={16} onChange={(v) => updateQueue(q.id, { maxConcurrent: v })} />
      </SettingRow>
      <SettingRow title="Retries per file" desc="Retries after a failure that can be retried; a failure that needs you stops at once.">
        <NumberInput label="Retries" value={q.retries} min={0} max={50} onChange={(v) => updateQueue(q.id, { retries: v })} />
      </SettingRow>
      <SettingRow title="Queue speed limit" desc="Shared by this queue's active files. The global limiter still applies on top.">
        <Switch label="Queue speed limit" checked={q.limitKiBps !== null} onChange={(v) => updateQueue(q.id, { limitKiBps: v ? 2048 : null })} />
        {q.limitKiBps !== null ? <NumberInput label="KiB per second" value={q.limitKiBps} min={16} max={1_048_576} onChange={(v) => updateQueue(q.id, { limitKiBps: v })} unit="KiB/s" /> : null}
      </SettingRow>
    </SettingsGroup>
  )
}

function SyncTab({ q, now }: { q: Queue; now: number }) {
  const files = useStore((s) => s.jobs.filter((j) => j.queue === q.id))
  return (
    <>
      <SettingsGroup title="Synchronization" note="A synchronization queue asks each server whether a finished file changed (by its validators) and downloads the new version beside the old one. Nothing is overwritten.">
        <SettingRow title="Check every" desc={q.lastStarted ? `Last checked ${fmtWhen(q.lastStarted, now)}.` : 'Not checked yet.'}>
          <NumberInput label="Hours" value={q.syncEveryHours} min={1} max={720} onChange={(v) => updateQueue(q.id, { syncEveryHours: v })} unit="hours" />
        </SettingRow>
      </SettingsGroup>
      <section class="sgroup">
        <h2 class="sgroup-title">Watched files</h2>
        {files.length ? (
          <div class="table-wrap">
            <table>
              <thead>
                <tr>
                  <th>File</th>
                  <th>Host</th>
                  <th>Last result</th>
                </tr>
              </thead>
              <tbody>
                {files.map((j) => (
                  <tr>
                    <td>{j.name}</td>
                    <td class="mono small">{j.host}</td>
                    <td class="muted">unchanged (validators matched)</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        ) : (
          <Empty>No files are watched.</Empty>
        )}
      </section>
    </>
  )
}

function Limiter() {
  const l = useStore((s) => s.limiter)
  const now = useStore((s) => s.now)
  const active = limiterActive(l, now)
  return (
    <>
      <SettingsGroup title="Global speed limit" note="The limit is shared by every active download. Queue limits apply on top of it; a file gets the tighter of the two shares.">
        <SettingRow title="Speed limiter" desc={l.enabled ? (active ? 'In force now.' : 'On, but outside its schedule right now.') : 'Off: downloads use the bandwidth the network gives them.'}>
          <Switch label="Speed limiter" checked={l.enabled} onChange={(v) => setLimiter({ enabled: v })} />
        </SettingRow>
        <SettingRow title="Limit" desc={`${fmtBytes(l.limitKiBps * 1024)}/s for everything Zeus downloads.`}>
          <NumberInput label="KiB per second" value={l.limitKiBps} min={16} max={10_485_760} onChange={(v) => setLimiter({ limitKiBps: v })} unit="KiB/s" />
        </SettingRow>
        <SettingRow title="Presets">
          {[512, 1024, 4096, 16384].map((k) => (
            <button type="button" class={`btn small ${l.limitKiBps === k ? 'primary' : ''}`} onClick={() => setLimiter({ limitKiBps: k, enabled: true })}>
              {fmtBytes(k * 1024)}/s
            </button>
          ))}
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Schedule" note="Outside the window downloads run unlimited. A window that ends earlier than it starts runs past midnight and belongs to the day it starts.">
        <SettingRow title="Only at certain times" desc="For example during working hours, so calls and meetings keep their bandwidth.">
          <Switch label="Only at certain times" checked={l.scheduleEnabled} onChange={(v) => setLimiter({ scheduleEnabled: v })} />
        </SettingRow>
        {l.scheduleEnabled ? (
          <>
            <SettingRow title="From and to">
              <TimeInput label="From" value={l.from} onChange={(v) => setLimiter({ from: v })} />
              <span class="muted">to</span>
              <TimeInput label="To" value={l.to} onChange={(v) => setLimiter({ to: v })} />
            </SettingRow>
            <SettingRow title="Days">
              <DayPicker label="Days" days={l.days} onChange={(d) => setLimiter({ days: d })} />
            </SettingRow>
          </>
        ) : null}
      </SettingsGroup>
      <SettingsGroup title="Per queue">
        {getState().queues.map((q) => (
          <SettingRow title={q.name} desc={q.limitKiBps === null ? 'No queue limit' : `${fmtBytes(q.limitKiBps * 1024)}/s shared by its active files`}>
            <a class="btn small" href={href('queues', { q: q.id, tab: 'limits' })}>
              Change
            </a>
          </SettingRow>
        ))}
      </SettingsGroup>
    </>
  )
}

export function Queues({ queueId, tab }: { queueId?: string; tab?: string }) {
  const queues = useStore((s) => s.queues)
  const jobs = useStore((s) => s.jobs)
  const now = useStore((s) => s.now)
  const [newName, setNewName] = useState('')
  const limiterTab = tab === 'limiter'
  const q = queues.find((x) => x.id === queueId) ?? (limiterTab ? undefined : queues[0])
  const sub = (tab === 'files' || tab === 'limits' || tab === 'sync' ? tab : 'schedule') as 'schedule' | 'files' | 'limits' | 'sync'
  const go = (patch: { q?: string; tab?: string }) => navigate(href('queues', { q: patch.q ?? q?.id, tab: patch.tab }))
  return (
    <div class="page">
      <PageHead title="Queues and scheduler" sub="Named queues with their own times, limits and finishing action, and one speed limiter for everything." />
      <div class="split-pane qpane">
        <div class="pane-list" role="listbox" aria-label="Queues">
          {queues.map((x) => {
            const waiting = jobs.filter((j) => j.queue === x.id && j.state !== 'completed' && j.state !== 'canceled').length
            const next = nextStart(x.schedule, now)
            return (
              <div class={`pane-item ${x.id === q?.id && !limiterTab ? 'on' : ''}`} role="option" aria-selected={x.id === q?.id && !limiterTab} tabIndex={0} onClick={() => go({ q: x.id, tab: 'schedule' })} onKeyDown={(e) => e.key === 'Enter' && go({ q: x.id, tab: 'schedule' })}>
                <Icon name={x.kind === 'sync' ? 'refresh' : 'queue'} />
                <span class="pi-text">
                  <span class="pi-name">{x.name}</span>
                  <span class="pi-meta">
                    {x.running ? 'running' : next ? `starts ${fmtWhen(next, now)}` : 'starts by hand'} · {waiting} waiting
                  </span>
                </span>
                {x.running ? <span class="dot run" aria-label="running" /> : null}
              </div>
            )
          })}
          <div class={`pane-item ${limiterTab ? 'on' : ''}`} role="option" aria-selected={limiterTab} tabIndex={0} onClick={() => navigate(href('queues', { tab: 'limiter' }))} onKeyDown={(e) => e.key === 'Enter' && navigate(href('queues', { tab: 'limiter' }))}>
            <Icon name="gauge" />
            <span class="pi-text">
              <span class="pi-name">Speed limiter</span>
              <span class="pi-meta">{getState().limiter.enabled ? `${fmtBytes(getState().limiter.limitKiBps * 1024)}/s` : 'off'}</span>
            </span>
          </div>
          <form
            class="pane-add-form"
            onSubmit={(e) => {
              e.preventDefault()
              const id = addQueue(newName)
              setNewName('')
              go({ q: id, tab: 'schedule' })
            }}
          >
            <input value={newName} aria-label="New queue name" placeholder="New queue name" onInput={(e) => setNewName((e.target as HTMLInputElement).value)} />
            <button type="submit" class="btn small">
              <Icon name="plus" /> New queue
            </button>
          </form>
        </div>
        <div class="pane-detail">
          {limiterTab ? (
            <Limiter />
          ) : q ? (
            <>
              <div class="pane-head">
                <div>
                  <div class="title">
                    {q.name} {q.running ? <Chip tone="accent">running</Chip> : <Chip>stopped</Chip>} {q.kind === 'sync' ? <Chip tone="info">synchronization</Chip> : null}
                  </div>
                  <div class="desc">{describeSchedule(q.schedule)}</div>
                </div>
                <div class="pane-tools">
                  {q.running ? (
                    <button type="button" class="btn small" onClick={() => stopQueue(q.id)}>
                      <Icon name="stop" /> Stop
                    </button>
                  ) : (
                    <button type="button" class="btn small primary" onClick={() => startQueue(q.id)}>
                      <Icon name="play" /> Start now
                    </button>
                  )}
                  {!q.builtin ? (
                    <button type="button" class="icon-btn" aria-label="Delete queue" title="Delete this queue; its files stay in the list" onClick={() => (deleteQueue(q.id), go({ q: 'main' }))}>
                      <Icon name="trash" />
                    </button>
                  ) : null}
                </div>
              </div>
              <label class="field">
                Name
                <input value={q.name} disabled={q.builtin} onChange={(e) => updateQueue(q.id, { name: (e.target as HTMLInputElement).value.trim() || q.name })} />
              </label>
              <Tabs
                value={sub}
                onChange={(t) => go({ tab: t })}
                tabs={[['schedule', 'Schedule'], ['files', 'Files in the queue'], ['limits', 'Limits'], ...(q.kind === 'sync' ? ([['sync', 'Synchronization']] as ['sync', string][]) : [])]}
              />
              {sub === 'schedule' ? <ScheduleTab q={q} now={now} /> : null}
              {sub === 'files' ? <FilesTab q={q} /> : null}
              {sub === 'limits' ? <LimitsTab q={q} /> : null}
              {sub === 'sync' && q.kind === 'sync' ? <SyncTab q={q} now={now} /> : null}
            </>
          ) : null}
        </div>
      </div>
      <Provenance>
        zeusd does not schedule queues yet; its documentation lists daemon scheduling as open work. In this release the queues, times and limits run in the app on sample downloads. {q ? `Queue: ${queueName(q.id)}.` : ''}
      </Provenance>
    </div>
  )
}
