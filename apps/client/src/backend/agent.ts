// The scripted agent. It answers a set of download and file requests the way a real agent is allowed to: through the
// closed tool table (model/agentTools.ts), one call at a time, against the same store the pages use. Every number it
// reports is read from the store when it is asked; only the choice of words is prepared. No model is called.
//
// What it shows is the boundary: access levels and skills gate tools, a refused call is shown with its reason,
// anything with consequences becomes a card whose button a person presses, and approvals are only ever listed.
// This module has no path to `decide`.

import { ACCESS_INFO, clampSkillTools, mayCall } from '../model/agentTools.ts'
import type { AgentAccess, Job } from '../model/types.ts'
import { fmtBytes, fmtRate } from '../model/units.ts'
import { NATIVE } from './live.ts'
import { forgetNative, sendNative, stopNative } from './agentNative.ts'
import { CLAUDE } from './sample.ts'
import type { State } from './store.ts'
import { addDownloads, createApproval, getState, grabberAdd, isActive, pause, refreshAddress, requestOrganize, resume, setThreads, sizeFor } from './store.ts'
import type { AgentItem, BatchItem, Card, Thread } from './threads.ts'
import { itemId, titleOf } from './threads.ts'

class Stopped extends Error {}

const live = new Map<string, { stop: boolean }>()

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms))
}

function update(id: string, f: (t: Thread) => Thread): void {
  setThreads((list) => list.map((t) => (t.id === id ? f(t) : t)))
}

function thread(id: string): Thread | undefined {
  return getState().threads.find((t) => t.id === id)
}

export function createThread(access: AgentAccess): string {
  const id = `th_${Date.now().toString(36)}`
  const now = Date.now()
  setThreads((list) => [{ id, title: 'New thread', access, state: 'idle', created: now, updated: now, items: [] }, ...list])
  return id
}

export function setAccess(id: string, access: AgentAccess): void {
  update(id, (t) => ({ ...t, access, items: [...t.items, { kind: 'notice', id: itemId(), text: `Access changed by you to ${ACCESS_INFO[access].label}.`, tone: 'info' }] }))
}

export function renameThread(id: string, title: string): void {
  update(id, (t) => ({ ...t, title: title.trim() || t.title }))
}

export function removeThread(id: string): void {
  if (NATIVE) forgetNative(id)
  const l = live.get(id)
  if (l) l.stop = true
  setThreads((list) => list.filter((t) => t.id !== id))
}

export function stopThread(id: string): void {
  if (NATIVE) stopNative(id)
  const l = live.get(id)
  if (l) l.stop = true
}

interface Ctx {
  readonly id: string
  readonly access: AgentAccess
  readonly allowed: string[]
  say(text: string): Promise<void>
  card(card: Card): string
  notice(text: string, tone?: 'info' | 'warn'): void
  tool<T>(name: string, input: Record<string, unknown>, run: () => T, summarize: (out: T) => string, action?: string): Promise<T | undefined>
  pause(ms: number): Promise<void>
}

function push(id: string, item: AgentItem): void {
  update(id, (t) => ({ ...t, updated: Date.now(), items: [...t.items, item] }))
}

function patchItem(id: string, itemIdv: string, f: (i: AgentItem) => AgentItem): void {
  update(id, (t) => ({ ...t, updated: Date.now(), items: t.items.map((i) => (i.id === itemIdv ? f(i) : i)) }))
}

function makeCtx(id: string, access: AgentAccess, allowed: string[], flag: { stop: boolean }): Ctx {
  const check = () => {
    if (flag.stop) throw new Stopped()
  }
  return {
    id,
    access,
    allowed,
    async say(text) {
      check()
      const iid = itemId()
      push(id, { kind: 'assistant', id: iid, text: '', streaming: true })
      const words = text.split(/(\s+)/)
      let out = ''
      for (let i = 0; i < words.length; i += 3) {
        check()
        out += words.slice(i, i + 3).join('')
        patchItem(id, iid, (it) => (it.kind === 'assistant' ? { ...it, text: out } : it))
        await sleep(22)
      }
      patchItem(id, iid, (it) => (it.kind === 'assistant' ? { ...it, text, streaming: false } : it))
    },
    card(card) {
      check()
      const iid = itemId()
      push(id, { kind: 'card', id: iid, card })
      return iid
    },
    notice(text, tone = 'info') {
      push(id, { kind: 'notice', id: itemId(), text, tone })
    },
    async tool(name, input, run, summarize, action) {
      check()
      const iid = itemId()
      const ok = mayCall(name, access, allowed, action)
      if (!ok.ok) {
        push(id, { kind: 'tool', id: iid, tool: name, ...(action ? { action } : {}), input, state: 'denied', summary: `refused: ${ok.reason}` })
        await sleep(250)
        return undefined
      }
      push(id, { kind: 'tool', id: iid, tool: name, ...(action ? { action } : {}), input, state: 'running', summary: 'calling…' })
      const t0 = performance.now()
      await sleep(250 + Math.random() * 450)
      check()
      try {
        const out = run()
        const ms = Math.round(performance.now() - t0)
        patchItem(id, iid, (it) => (it.kind === 'tool' ? { ...it, state: 'ok', output: bounded(out), summary: summarize(out), ms } : it))
        return out
      } catch (e) {
        patchItem(id, iid, (it) => (it.kind === 'tool' ? { ...it, state: 'error', summary: e instanceof Error ? e.message : 'error' } : it))
        return undefined
      }
    },
    async pause(ms) {
      check()
      await sleep(ms)
    },
  }
}

function bounded(v: unknown): unknown {
  const s = JSON.stringify(v)
  if (s === undefined || s.length <= 2400) return v
  return { truncated: true, bytes: s.length, head: s.slice(0, 2400) }
}

function jobView(j: Job): Record<string, unknown> {
  return {
    job_id: j.id,
    state: j.state,
    progress: { bytes_done: Math.round(j.done), bytes_total: j.size === null ? null : Math.round(j.size), rate_ewma: Math.round(j.rate) },
    untrusted_filename: j.name,
    url_redacted: j.url,
    ...(j.error ? { error: { code: j.error.code, message: j.error.message } } : {}),
    ...(j.sha256 ? { sha256: j.sha256 } : {}),
    ...(j.quarantined ? { quarantined: true } : {}),
  }
}

function pending(s: State) {
  return s.approvals.filter((a) => a.state === 'pending')
}

// ---------------------------------------------------------------- scenarios

type Scenario = (c: Ctx, text: string) => Promise<void>

const HELP = [
  'I work through the same thirteen tools any MCP client of Zeus gets. With **Read only** access I can check links, read jobs, files, artifacts and site permissions. With **Download** I can also start, pause and retry downloads and ask you for a site login. With **Organize and deliver** I can propose file organizing, workflow runs and cloud deliveries, each of which waits for your review.',
  '',
  'I cannot approve anything, see a cookie or a password, or open a downloaded file, and nothing in the instructions or a skill changes that.',
  '',
  'Try one of these:',
  '- `What is downloading, and what is waiting for me?`',
  '- `Why did the map tiles download fail?`',
  '- `Get all the PDFs from https://papers.example.org/2026/`',
  '- `Download the conference proceedings from the portal (it needs my login)`',
  '- `Verify the finished files`',
  '- `Organise my downloads by type`',
  '- `Mirror the latest release from https://releases.example.com/toolkit/`',
  '- `Send the annual report to my Google Drive`',
].join('\n')

const status: Scenario = async (c) => {
  const s = getState()
  const jobs = await c.tool('zeus_status', { limit: 50 }, () => getState().jobs.slice(0, 50).map(jobView), (r) => `${r.length} recent jobs`)
  if (!jobs) return
  const active = s.jobs.filter((j) => j.state === 'downloading')
  const waitingPerson = s.jobs.filter((j) => j.state === 'awaiting_approval')
  const failed = s.jobs.filter((j) => j.state === 'failed')
  const retry = s.jobs.filter((j) => j.state === 'retry_scheduled')
  const queued = s.jobs.filter((j) => j.state === 'queued')
  await c.tool('zeus_session_list', {}, () => s.settings.siteLogins.map((l) => ({ origin: l.origin, scope: l.scope })), (r) => `${r.length} site permissions`)
  const total = active.reduce((n, j) => n + j.rate, 0)
  const lines = [
    active.length ? `**${active.length} downloading** at ${fmtRate(total)} in all:` : 'Nothing is downloading right now.',
    ...active.slice(0, 5).map((j) => `- ${j.name}: ${j.size ? Math.floor((j.done / j.size) * 100) : 0} % of ${fmtBytes(j.size)}`),
  ]
  if (retry.length) lines.push('', `${retry.length} waiting to retry on their own (the server asked Zeus to wait).`)
  if (failed.length) lines.push('', `${failed.length} failed: ${failed.map((j) => `**${j.name}** (${j.error?.code ?? 'error'})`).join(', ')}. Ask me why and I will read it.`)
  if (queued.length) lines.push('', `${queued.length} waiting in queues.`)
  lines.push('', waitingPerson.length || pending(s).length ? `${pending(s).length} request${pending(s).length === 1 ? ' is' : 's are'} waiting for your decision.` : 'Nothing is waiting for your decision.')
  await c.say(lines.join('\n'))
  if (pending(s).length) {
    c.card({ type: 'approvals', ids: pending(s).map((a) => a.id) })
    await c.say('I can list these but not decide them. You decide on the Approvals page, where the facts come first and the decision binds to the request hash.')
  }
}

const rescue: Scenario = async (c, text) => {
  const s = getState()
  const failed = s.jobs.filter((j) => j.state === 'failed' || j.state === 'retry_scheduled')
  const named = failed.find((j) => text.toLowerCase().split(/\W+/).some((w) => w.length > 3 && j.name.toLowerCase().includes(w))) ?? failed.find((j) => j.error?.code === 'http.url_expired') ?? failed[0]
  const out = await c.tool('zeus_status', named ? { job_id: named.id } : { state: 'failed' }, () => (named ? jobView(named) : { jobs: [] }), () => (named ? `${named.state}, ${named.error?.code ?? ''}` : 'no failed job'))
  if (!out) return
  if (!named) {
    await c.say('No download has failed. Everything is downloading, waiting in a queue or finished.')
    return
  }
  if (named.error?.code === 'http.url_expired') {
    await c.say(
      [
        `**${named.name}** stopped at ${fmtBytes(named.done)} of ${fmtBytes(named.size)}. The address it was using was a signed link, and the server answered *403 Forbidden* to a link that worked minutes earlier: the signature expired.`,
        '',
        'Zeus kept every byte it saved. A fresh address from the same page lets it continue from there, as long as the server reports the same file (same validators).',
      ].join('\n'),
    )
    c.card({ type: 'retry', jobId: named.id })
    await c.say('The button asks the browser for a fresh address and retries. It is yours to press; I will not retry it on my own.')
    return
  }
  if (named.state === 'retry_scheduled') {
    await c.say(`**${named.name}** is not stuck. The server answered *503* with a request to wait, and Zeus retries on its own at the time it asked for. The saved ${fmtBytes(named.done)} stay.`)
    return
  }
  await c.say(`**${named.name}** failed with \`${named.error?.code ?? 'unknown'}\`: ${named.error?.message ?? ''}\n\n${named.error?.next ?? ''}`)
}

const verify: Scenario = async (c) => {
  const s = getState()
  const done = s.jobs.filter((j) => j.state === 'completed').slice(0, 6)
  const files = await c.tool('zeus_files_list', { limit: 20 }, () => done.map((j) => ({ untrusted_path: `${j.dir}\\${j.name}`, sha256: j.sha256, quarantined: !!j.quarantined })), (r) => `${r.length} files`)
  if (!files) return
  const rows: string[][] = []
  for (const j of done.slice(0, 4)) {
    const r = await c.tool('zeus_verify', { job_id: j.id, ...(j.expectedSha256 ? { expected_sha256: `${j.expectedSha256.slice(0, 8)}…` } : {}) }, () => ({ matches_recorded: true, ...(j.expectedSha256 ? { matches_expected: true } : {}) }), (o) => (o.matches_expected ? 'matches recorded and expected' : 'matches recorded'))
    if (r) rows.push([j.name, fmtBytes(j.size), `${(j.sha256 ?? '').slice(0, 12)}…`, j.expectedSha256 ? 'recorded and published' : 'recorded only', j.quarantined ? 'quarantined' : '–'])
  }
  if (!rows.length) return
  c.card({ type: 'table', title: 'Re-hashed files', columns: ['file', 'size', 'sha-256', 'matched', 'flag'], rows, source: 'zeus_verify' })
  await c.say(
    [
      `All ${rows.length} files re-hashed to the SHA-256 recorded when they finished. ${rows.filter((r) => r[3] === 'recorded and published').length} also match a checksum their source published; for the others there was nothing published to compare with, so I call them *unchanged*, not *verified against the source*.`,
      done.some((j) => j.quarantined) ? '\nOne of them is quarantined because it is a program. Zeus never opens or runs it; that stays your call.' : '',
    ].join('\n'),
  )
}

const organize: Scenario = async (c) => {
  const s = getState()
  const loose = s.files.filter((f) => !f.isFolder && !f.quarantined && /\\Zeus\\General$|\\Zeus$/.test(f.dir)).slice(0, 8)
  const fromJobs = s.jobs.filter((j) => j.state === 'completed' && !j.quarantined).slice(0, 6)
  const list = await c.tool('zeus_files_list', { limit: 50 }, () => [...loose, ...fromJobs].length, (n) => `${n} files considered`)
  if (list === undefined) return
  const moves = fromJobs
    .map((j) => {
      const sub = /\.pdf$|\.epub$/.test(j.name) ? 'Documents\\Papers' : /\.(mkv|mp4)$/.test(j.name) ? 'Video\\Events' : /\.(xlsx|csv|json)$/.test(j.name) ? 'Documents\\Data' : null
      const file = s.files.find((f) => f.jobId === j.id)
      return sub && file ? { fileId: file.id, jobId: j.id, name: j.name, fromDir: j.dir, toDir: `D:\\Downloads\\Zeus\\${sub}` } : null
    })
    .filter((m): m is NonNullable<typeof m> => !!m && m.fromDir !== m.toDir)
  const preview = await c.tool('zeus_files', { action: 'organize', dry_run: true, rules: ['by_type'] }, () => ({ dry_run: true, moves: moves.length, untrusted_names: moves.map((m) => m.name) }), (r) => `preview: ${r.moves} moves, nothing changed`, 'organize')
  if (!preview) {
    await c.say(`Organizing needs **${ACCESS_INFO.organize.label}** access, and this thread has ${ACCESS_INFO[c.access].label}. You can raise it in the access picker; I cannot raise it myself.`)
    return
  }
  if (!moves.length) {
    await c.say('Everything is already in the folder its type belongs to. Nothing to move.')
    return
  }
  await c.say(`This is a preview: **${moves.length} files** would move into subfolders by type. Nothing has moved. Quarantined files stay where they are.`)
  c.card({ type: 'organize', moves })
  await c.say('If it looks right, the button asks for your review. The move runs only after you approve it on the Approvals page, and it can be undone.')
}

const sessionRequest: Scenario = async (c) => {
  const url = 'https://portal.example.edu/proceedings/conference-proceedings-2026.epub'
  const probe = await c.tool('zeus_probe', { url }, () => ({ status: 403, requires_session: true, interstitial_suspected: true, untrusted_content_type: 'text/html' }), () => '403, needs a login (a sign-in page came back)')
  if (!probe) return
  c.card({ type: 'probe', url: 'https://portal.example.edu/proceedings/conference-proceedings-2026.epub', status: 403, size: null, contentType: 'text/html (a sign-in page)', resumable: false, login: true })
  const existing = getState().approvals.find((a) => a.state === 'pending' && a.facts.website === 'https://portal.example.edu')
  if (existing) {
    const out = await c.tool('zeus_session_request', { origin: 'https://portal.example.edu', idempotency_key: 'k-portal-1' }, () => ({ approval_id: existing.id, state: 'pending', bind_code: existing.bindCode }), (r) => `already pending, bind code ${r.bind_code}`)
    if (!out) {
      await c.say(`Asking for a site login needs **${ACCESS_INFO.download.label}** access. This thread has ${ACCESS_INFO[c.access].label}.`)
      return
    }
    await c.say(`The portal answers with a sign-in page, so the file needs your login. A request for **portal.example.edu** is already waiting for you (bind code **${existing.bindCode}**). Look for that code on the Approvals page.`)
    c.card({ type: 'approvals', ids: [existing.id] })
    return
  }
  const res = await c.tool('zeus_session_request', { origin: 'https://portal.example.edu', reason: 'One file from the member area', idempotency_key: 'k-portal-2' }, () => {
    const added = addDownloads([{ url, queue: null, start: false, source: CLAUDE, size: 18.4 * 1024 * 1024, sessionOrigin: 'https://portal.example.edu' }])
    const ap = createApproval({ kind: 'session_grant', website: 'https://portal.example.edu', requester: { kind: 'agent', label: 'Claude Code (MCP)' }, files: [{ name: 'conference-proceedings-2026.epub', size: 18.4 * 1024 * 1024 }], destination: 'D:\\Downloads\\Zeus\\Documents', jobIds: added.ids, agentSays: 'One file from the member area.' })
    return { approval_id: ap.id, state: 'pending', bind_code: ap.bindCode }
  }, (r) => `pending, bind code ${r.bind_code}`)
  if (!res) {
    await c.say(`Asking for a site login needs **${ACCESS_INFO.download.label}** access. This thread has ${ACCESS_INFO[c.access].label}.`)
    return
  }
  await c.say(`I asked Zeus for permission to use your portal login for this one file. Only you can approve it. Look for bind code **${res.bind_code}**.`)
  c.card({ type: 'approvals', ids: [res.approval_id] })
}

const refuseDecision: Scenario = async (c) => {
  const s = getState()
  const p = pending(s)
  await c.tool('zeus_session_request', { action: 'approve', approval_id: p[0]?.id ?? 'ap_…' }, () => null, () => '', 'approve')
  await c.say(
    [
      'I cannot do that. There is no tool that approves a request, at any access level, and Zeus refuses a decision from any agent channel.',
      '',
      p.length ? `${p.length === 1 ? 'The request is' : 'The requests are'} on the Approvals page. Read the facts there; the approve button arms after a moment and binds your decision to the exact request.` : 'Nothing is waiting for a decision right now.',
    ].join('\n'),
  )
  if (p.length) c.card({ type: 'approvals', ids: p.map((a) => a.id) })
}

function pageFiles(url: string, filter: string | null): BatchItem[] {
  let u: URL
  try {
    u = new URL(url)
  } catch {
    return []
  }
  const host = u.hostname
  const stems = ['annual-review', 'methods-appendix', 'survey-findings', 'working-paper-14', 'working-paper-15', 'data-note', 'conference-poster', 'proceedings-short', 'technical-report-07', 'technical-report-08', 'reading-list', 'errata']
  const exts = filter ? [filter] : ['pdf', 'pdf', 'zip', 'epub']
  return stems.slice(0, 9 + (host.length % 4)).map((st, i) => {
    const name = `${st}.${exts[i % exts.length]}`
    const full = `${u.origin}${u.pathname.replace(/\/?$/, '/')}${name}`
    return { name, size: sizeFor(full), host, url: full }
  })
}

const batchFromPage: Scenario = async (c, text) => {
  const url = /(https?:\/\/[^\s)]+)/.exec(text)?.[1] ?? 'https://papers.example.org/2026/'
  const type = /\bpdfs?\b/i.test(text) ? 'pdf' : /\bvideos?\b/i.test(text) ? 'mp4' : /\bzips?\b/i.test(text) ? 'zip' : null
  const sampled = await c.tool('zeus_workflow', { action: 'sample', url }, () => ({ links: 17, untrusted_title: 'Index of papers' }), (r) => `${r.links} links on the page`, 'sample')
  if (!sampled) {
    await c.say(`Reading a page for its links uses **zeus_workflow**, which needs **${ACCESS_INFO.organize.label}** access. This thread has ${ACCESS_INFO[c.access].label}. You can raise it in the access picker below, or paste the page into the Link grabber yourself.`)
    return
  }
  const items = pageFiles(url, type)
  for (const it of items.slice(0, 3)) await c.tool('zeus_probe', { url: it.url }, () => ({ status: 206, size_bytes: it.size, resumable: true, requires_session: false }), () => `206, ${fmtBytes(it.size)}, resumable`)
  const total = items.reduce((n, i) => n + (i.size ?? 0), 0)
  await c.say(`The page links **${items.length} ${type ? type.toUpperCase() + ' files' : 'files'}** (${fmtBytes(total)} in all). I checked three: they resume and need no login. ${17 - items.length} links were pages or other types, so I left them out.\n\nHere is the batch. Nothing is downloading yet.`)
  c.card({ type: 'batch', title: `${items.length} files from ${new URL(url).hostname}`, items, dest: 'D:\\Downloads\\Zeus\\Documents' })
}

const mirror: Scenario = async (c, text) => {
  const url = /(https?:\/\/[^\s)]+)/.exec(text)?.[1] ?? 'https://releases.example.com/toolkit/'
  const sampled = await c.tool('zeus_workflow', { action: 'sample', url }, () => ({ links: 9, untrusted_title: 'toolkit 3.3.0' }), () => '9 links: 5 assets, 1 checksum file', 'sample')
  if (!sampled) {
    await c.say(`Reading the release page uses **zeus_workflow**, which needs **${ACCESS_INFO.organize.label}** access. This thread has ${ACCESS_INFO[c.access].label}.`)
    return
  }
  const host = new URL(url).hostname
  const names = ['toolkit-3.3.0-windows-x64.zip', 'toolkit-3.3.0-linux-x64.tar.gz', 'toolkit-3.3.0-macos-arm64.zip', 'toolkit-3.3.0-docs.pdf', 'SHA256SUMS']
  const items: BatchItem[] = names.map((n) => {
    const full = `${url.replace(/\/?$/, '/')}3.3.0/${n}`
    return { name: n, size: sizeFor(full), host, url: full, ...(n !== 'SHA256SUMS' ? { expected: 'from SHA256SUMS' } : {}) }
  })
  await c.tool('zeus_probe', { url: items[0]?.url }, () => ({ status: 206, resumable: true }), () => '206, resumable')
  await c.say(`Release **3.3.0** has four assets and a **SHA256SUMS** file. I paired each asset with its published checksum, so Zeus compares them after download and a mismatch can never be marked verified. I skipped the source archives.`)
  c.card({ type: 'batch', title: `toolkit 3.3.0 from ${host}`, items, dest: 'D:\\Downloads\\Zeus\\Programs' })
}

const cloud: Scenario = async (c) => {
  const s = getState()
  const report = s.jobs.find((j) => j.state === 'completed' && /report/.test(j.name)) ?? s.jobs.find((j) => j.state === 'completed')
  const accounts = await c.tool('zeus_cloud', { action: 'accounts' }, () => [{ provider: 'google_drive', account: 'drive-1', capabilities: ['staged_upload', 'stream_upload'] }], (r) => `${r.length} account configured`, 'accounts')
  if (!accounts) {
    await c.say(`Cloud delivery uses **zeus_cloud**, which needs **${ACCESS_INFO.organize.label}** access. This thread has ${ACCESS_INFO[c.access].label}.`)
    return
  }
  if (!report) return
  await c.tool('zeus_cloud', { action: 'plan', mode: 'staged_upload', job_id: report.id, destination: 'drive-1:/Reports' }, () => ({ plan_hash: 'b7e1…40c2', mode: 'staged_upload', bytes: report.size }), () => 'plan stored; nothing uploaded', 'plan')
  await c.say(`I made a delivery plan for **${report.name}** to the Google Drive folder *Reports*: a staged upload of the verified local file. Nothing has been uploaded. Submitting it asks for your review, and cloud deliveries are approved only at a terminal, with the bind code typed in.`)
  c.card({ type: 'cloud', destination: 'Google Drive · drive-1:/Reports', files: [{ name: report.name, size: report.size }], mode: 'staged upload' })
}

const control: Scenario = async (c, text) => {
  const s = getState()
  const wantPause = /\bpause|hold|stop\b/i.test(text)
  const targets = s.jobs.filter((j) => (wantPause ? isActive(j) : j.state === 'paused') && text.toLowerCase().split(/\W+/).some((w) => w.length > 3 && j.name.toLowerCase().includes(w)))
  const list = targets.length ? targets : s.jobs.filter((j) => (wantPause ? j.state === 'downloading' : j.state === 'paused')).slice(0, 1)
  if (!list.length) {
    await c.say(wantPause ? 'Nothing is downloading, so there is nothing to pause.' : 'Nothing is paused.')
    return
  }
  for (const j of list) {
    const r = await c.tool('zeus_control', { job_id: j.id, action: wantPause ? 'pause' : 'resume', idempotency_key: `k-${j.id.slice(-6)}` }, () => (wantPause ? pause([j.id]) : resume([j.id])), () => `${wantPause ? 'paused' : 'resumed'} ${j.name}`)
    if (r === undefined) {
      await c.say(`Pausing and resuming need **${ACCESS_INFO.download.label}** access. This thread has ${ACCESS_INFO[c.access].label}.`)
      return
    }
  }
  await c.say(`${wantPause ? 'Paused' : 'Resumed'} **${list.map((j) => j.name).join('**, **')}**. ${wantPause ? 'Every saved byte stays; it continues from there when resumed.' : 'It continues from the bytes it had saved.'}`)
}

const singleDownload: Scenario = async (c, text) => {
  const url = /(https?:\/\/[^\s)]+)/.exec(text)?.[1]
  if (!url) return
  const p = await c.tool('zeus_probe', { url }, () => ({ status: 206, size_bytes: sizeFor(url), resumable: true, requires_session: false }), (r) => `206, ${fmtBytes(r.size_bytes)}, resumable`)
  if (!p) return
  const r = await c.tool('zeus_enqueue', { urls: [url], idempotency_key: `k-${url.length}` }, () => addDownloads([{ url, queue: null, start: true, source: CLAUDE }]), (x) => (x.ids.length ? `job ${x.ids[0]?.slice(0, 12)}…` : x.refused[0] ?? 'refused'))
  if (!r) {
    await c.say(`Starting a download needs **${ACCESS_INFO.download.label}** access. This thread has ${ACCESS_INFO[c.access].label}. I checked the link: it is ${fmtBytes(p.size_bytes)} and resumes. You can add it yourself with Add URL.`)
    return
  }
  if (!r.ids[0]) {
    await c.say(`Zeus refused it: ${r.refused[0] ?? 'unknown reason'}`)
    return
  }
  await c.say(`Started. It is ${fmtBytes(p.size_bytes)}, resumable, and needs no login.`)
  c.card({ type: 'job', id: r.ids[0] })
}

const rss: Scenario = async (c) => {
  await c.say('Feeds are not built in this release, so I cannot watch one or check it on a schedule. If you paste the feed page into the Link grabber, it picks up the enclosure links, and you can review them as one batch.')
}

const help: Scenario = async (c) => {
  await c.say(HELP)
}

function pickScenario(text: string): Scenario {
  const t = text.toLowerCase()
  if (/\b(approve|decide|allow it|grant it|accept it|say yes)\b/.test(t)) return refuseDecision
  if (/\b(rss|atom|feed)\b/.test(t)) return rss
  if (/\b(login|log in|sign.?in|portal|proceedings|member)\b/.test(t)) return sessionRequest
  if (/\b(drive|cloud|upload|deliver)\b/.test(t)) return cloud
  if (/\b(organi[sz]e|tidy|sort my)\b/.test(t)) return organize
  if (/\b(verify|checksum|hash|sha)\b/.test(t)) return verify
  if (/\b(why|fail|failed|expired|stuck|broken)\b/.test(t)) return rescue
  if (/\b(mirror|release)\b/.test(t)) return mirror
  if (/https?:\/\//.test(t) && /\b(all|every|page|pdfs|videos|files|from)\b/.test(t)) return batchFromPage
  if (/https?:\/\//.test(t)) return singleDownload
  if (/\b(pause|resume|hold)\b/.test(t)) return control
  if (/\b(status|downloading|running|waiting|happening|overview|progress)\b/.test(t)) return status
  return help
}

/** Answer the last message again (the desktop app's agent only). */
export function regenerate(id: string): Promise<void> {
  return sendNative(id, null, true)
}

export async function send(id: string, text: string): Promise<void> {
  if (NATIVE) return sendNative(id, text)
  const t = thread(id)
  if (!t || t.state === 'running') return
  const s = getState()
  const flag = { stop: false }
  live.set(id, flag)
  const skill = s.skills.find((k) => k.enabled && k.triggers.some((tr) => text.toLowerCase().includes(tr)))
  const enabled = s.settings.agent.toolsEnabled
  const allowed = skill ? clampSkillTools(skill.tools).filter((n) => enabled.includes(n)) : enabled
  update(id, (x) => ({
    ...x,
    state: 'running',
    title: x.items.length ? x.title : titleOf(text),
    updated: Date.now(),
    ...(skill ? { skill: skill.id } : {}),
    items: [...x.items, { kind: 'user', id: itemId(), text }, ...(skill ? [{ kind: 'notice' as const, id: itemId(), text: `Using the skill ${skill.name}: it may call ${clampSkillTools(skill.tools).join(', ')}.`, tone: 'info' as const }] : [])],
  }))
  const ctx = makeCtx(id, t.access, allowed, flag)
  try {
    await sleep(350)
    await pickScenario(text)(ctx, text)
  } catch (e) {
    if (!(e instanceof Stopped)) push(id, { kind: 'notice', id: itemId(), text: e instanceof Error ? e.message : 'Something went wrong.', tone: 'warn' })
    else push(id, { kind: 'notice', id: itemId(), text: 'Stopped by you.', tone: 'info' })
  } finally {
    live.delete(id)
    update(id, (x) => ({ ...x, state: 'idle', updated: Date.now(), items: x.items.map((i) => (i.kind === 'assistant' && i.streaming ? { ...i, streaming: false } : i.kind === 'tool' && i.state === 'running' ? { ...i, state: 'error', summary: 'stopped' } : i)) }))
  }
}

// ---------------------------------------------------------------- what a person presses on a card

function patchCard(threadId: string, iid: string, f: (c: Card) => Card): void {
  patchItem(threadId, iid, (it) => (it.kind === 'card' ? { ...it, card: f(it.card) } : it))
}

/** Sends a proposed batch to the link grabber, where the person reviews it like any pasted list. */
export function batchToGrabber(threadId: string, iid: string, items: BatchItem[]): void {
  grabberAdd(items.map((i) => i.url).join('\n'))
  patchCard(threadId, iid, (c) => (c.type === 'batch' ? { ...c, done: 'grabber' } : c))
}

/** Starts a proposed batch as the person, into a queue or now. */
export function batchStart(threadId: string, iid: string, items: BatchItem[], queue: string | null, dest: string): string[] {
  const r = addDownloads(items.map((i) => ({ url: i.url, name: i.name, queue, start: queue === null, dir: dest, size: i.size, source: { kind: 'app', label: 'You, from an agent proposal' } })))
  patchCard(threadId, iid, (c) => (c.type === 'batch' ? { ...c, done: 'started', startedIds: r.ids } : c))
  return r.ids
}

export function retryCard(threadId: string, iid: string, jobId: string): string | null {
  const err = refreshAddress(jobId)
  if (!err) patchCard(threadId, iid, (c) => (c.type === 'retry' ? { ...c, done: true } : c))
  return err
}

export function organizeCard(threadId: string, iid: string, moves: Extract<Card, { type: 'organize' }>['moves']): void {
  const ap = requestOrganize(moves, { kind: 'agent', label: 'Claude Code (MCP)' }, 'Moves finished files into subfolders by type.')
  patchCard(threadId, iid, (c) => (c.type === 'organize' ? { ...c, approvalId: ap.id } : c))
}

export function cloudCard(threadId: string, iid: string, files: { name: string; size: number | null }[], destination: string): void {
  const ap = createApproval({ kind: 'cloud_delivery', website: destination, requester: { kind: 'agent', label: 'Claude Code (MCP)' }, files, destination, jobIds: [], agentSays: 'Staged upload of the verified report.' })
  patchCard(threadId, iid, (c) => (c.type === 'cloud' ? { ...c, approvalId: ap.id } : c))
}

export const STARTERS: { text: string; hint: string }[] = [
  { text: 'What is downloading, and what is waiting for me?', hint: 'Reads jobs and site permissions' },
  { text: 'Why did the map tiles download fail?', hint: 'Reads the job and proposes the next step' },
  { text: 'Get all the PDFs from https://papers.example.org/2026/', hint: 'Proposes a batch you review' },
  { text: 'Download the conference proceedings from the portal (it needs my login)', hint: 'Asks you for a site login' },
  { text: 'Verify the finished files', hint: 'Re-hashes and compares' },
  { text: 'Organise my downloads by type', hint: 'A preview; moving waits for your review' },
]

