// The Zeus pre-alpha service: the parts of the client that zeusd does not implement yet, stored per visitor sandbox.
//
// It is not part of Zeus. zeusd and zeus-web-bridge serve downloads, files, artifacts, approvals listing and workflow
// reports; this service keeps everything else the client shows so that every setting and click survives a reload:
// queues and their schedules, the speed limiter, categories and file types, link-grabber lists, site-grabber plans
// (it crawls the sample sites for real), agent threads and skills, connector choices, the provider ladder order
// (never a key), bookmarks and search history, every Settings section, and the search index over the sandbox's real
// files. Approvals for plans and file moves that this service holds are decided here, by the person at the page,
// with the same rules as spec 07 (request hash binding, arming delay, offered scopes); they are labelled as such.
//
// Requests arrive only from the front proxy, which authenticates the visitor with zeus-web-bridge (auth_request) and
// passes the sandbox id in `x-zeus-session`. The service listens on the internal network only and has no route to
// the internet; the site grabber reaches only the fixture hosts in the catalogue.
//
// Node 24, no dependencies: node:http and node:sqlite.

import { createHash } from 'node:crypto'
import { createServer } from 'node:http'
import { readdirSync, readFileSync, statSync } from 'node:fs'
import { join, relative } from 'node:path'
import { DatabaseSync } from 'node:sqlite'

const PORT = Number(process.env.SVC_PORT ?? 8081)
const DB_PATH = process.env.SVC_DB ?? '/data/svc.sqlite'
const ROOT = process.env.SVC_DOWNLOADS ?? '/data/downloads'
const CATALOGUE = process.env.SVC_CATALOGUE ?? '/fixtures/catalogue.json'
const TTL_MS = Number(process.env.SVC_TTL_HOURS ?? 24) * 3_600_000
const MAX_SLICE = 512 * 1024
const MAX_SESSION_BYTES = 4 * 1024 * 1024
const ARMING_MS = 750

/** The state slices the client may store, and nothing else. */
const SLICES = new Set([
  'queues', 'limiter', 'settings', 'skills', 'threads', 'bookmarks', 'history', 'connectors', 'projects', 'grabber',
  'jobmeta', 'pending', 'killswitch', 'prefs', 'keybindings',
])

const catalogue = (() => {
  try {
    return JSON.parse(readFileSync(CATALOGUE, 'utf8'))
  } catch {
    return { hosts: [], sizes: {}, samples: [] }
  }
})()
const fixtureHosts = new Set((catalogue.hosts ?? []).map((h) => String(h).toLowerCase()))

const db = new DatabaseSync(DB_PATH)
db.exec(`
  PRAGMA journal_mode = WAL;
  CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY, first_seen INTEGER NOT NULL);
  CREATE TABLE IF NOT EXISTS state (session TEXT NOT NULL, slice TEXT NOT NULL, json TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (session, slice));
  CREATE TABLE IF NOT EXISTS approvals (session TEXT NOT NULL, id TEXT NOT NULL, json TEXT NOT NULL, updated INTEGER NOT NULL, PRIMARY KEY (session, id));
  CREATE TABLE IF NOT EXISTS index_files (session TEXT NOT NULL, path TEXT NOT NULL, size INTEGER NOT NULL, mtime INTEGER NOT NULL, is_dir INTEGER NOT NULL, PRIMARY KEY (session, path));
  CREATE TABLE IF NOT EXISTS index_meta (session TEXT PRIMARY KEY, generation INTEGER NOT NULL, last_scan INTEGER NOT NULL, files INTEGER NOT NULL);
`)

const q = {
  touch: db.prepare('INSERT OR IGNORE INTO sessions (id, first_seen) VALUES (?, ?)'),
  firstSeen: db.prepare('SELECT first_seen FROM sessions WHERE id = ?'),
  slices: db.prepare('SELECT slice, json, updated FROM state WHERE session = ?'),
  size: db.prepare('SELECT COALESCE(SUM(LENGTH(json)), 0) AS n FROM state WHERE session = ? AND slice != ?'),
  put: db.prepare('INSERT INTO state (session, slice, json, updated) VALUES (?, ?, ?, ?) ON CONFLICT (session, slice) DO UPDATE SET json = excluded.json, updated = excluded.updated'),
  clear: db.prepare('DELETE FROM state WHERE session = ?'),
  approvals: db.prepare('SELECT json FROM approvals WHERE session = ? ORDER BY updated DESC LIMIT 200'),
  approval: db.prepare('SELECT json FROM approvals WHERE session = ? AND id = ?'),
  putApproval: db.prepare('INSERT INTO approvals (session, id, json, updated) VALUES (?, ?, ?, ?) ON CONFLICT (session, id) DO UPDATE SET json = excluded.json, updated = excluded.updated'),
  countApprovals: db.prepare("SELECT COUNT(*) AS n FROM approvals WHERE session = ?"),
  indexFiles: db.prepare('SELECT path, size, mtime, is_dir FROM index_files WHERE session = ? ORDER BY path LIMIT 20000'),
  indexMeta: db.prepare('SELECT generation, last_scan, files FROM index_meta WHERE session = ?'),
  indexClear: db.prepare('DELETE FROM index_files WHERE session = ?'),
  indexPut: db.prepare('INSERT OR REPLACE INTO index_files (session, path, size, mtime, is_dir) VALUES (?, ?, ?, ?, ?)'),
  indexMetaPut: db.prepare('INSERT INTO index_meta (session, generation, last_scan, files) VALUES (?, 1, ?, ?) ON CONFLICT (session) DO UPDATE SET generation = generation + 1, last_scan = excluded.last_scan, files = excluded.files'),
  expired: db.prepare('SELECT id FROM sessions WHERE first_seen < ?'),
}

function expire() {
  const cutoff = Date.now() - TTL_MS
  for (const { id } of q.expired.all(cutoff)) {
    for (const t of ['state', 'approvals', 'index_files', 'index_meta']) db.prepare(`DELETE FROM ${t} WHERE session = ?`).run(id)
    db.prepare('DELETE FROM sessions WHERE id = ?').run(id)
  }
}
setInterval(expire, 10 * 60_000).unref()

// ---------------------------------------------------------------- helpers

function send(res, status, body) {
  const text = JSON.stringify(body)
  res.writeHead(status, { 'content-type': 'application/json', 'cache-control': 'no-store', 'content-length': Buffer.byteLength(text) })
  res.end(text)
}

function fail(res, status, code, message) {
  send(res, status, { error: { code, message } })
}

async function body(req, limit = MAX_SLICE) {
  const chunks = []
  let n = 0
  for await (const c of req) {
    n += c.length
    if (n > limit) throw Object.assign(new Error('too large'), { status: 413 })
    chunks.push(c)
  }
  const text = Buffer.concat(chunks).toString('utf8')
  if (!text) return null
  try {
    return JSON.parse(text)
  } catch {
    throw Object.assign(new Error('not json'), { status: 400 })
  }
}

/** Canonical JSON as the client computes it (sorted keys, integers only). */
function canonical(v) {
  if (v === null || typeof v === 'boolean') return JSON.stringify(v)
  if (typeof v === 'number') {
    if (!Number.isSafeInteger(v)) throw new Error('integers only')
    return String(v)
  }
  if (typeof v === 'string') return JSON.stringify(v)
  if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`
  if (typeof v === 'object') {
    const keys = Object.keys(v).filter((k) => v[k] !== undefined).sort()
    return `{${keys.map((k) => `${JSON.stringify(k)}:${canonical(v[k])}`).join(',')}}`
  }
  throw new Error('unsupported')
}

const sha256 = (s) => createHash('sha256').update(s, 'utf8').digest('hex')

/** Never store something that looks like a provider key, even if a client tried. */
function looksSecret(text) {
  return /\b(sk-[A-Za-z0-9_-]{16,}|sk-ant-[A-Za-z0-9_-]{16,}|AIza[0-9A-Za-z_-]{20,}|ZEUS-CANARY-)/.test(text)
}

// ---------------------------------------------------------------- the index over the sandbox's real files

function scan(session) {
  const base = join(ROOT, 's', session)
  const rows = []
  const walk = (dir, depth) => {
    if (depth > 8 || rows.length > 20000) return
    let entries
    try {
      entries = readdirSync(dir, { withFileTypes: true })
    } catch {
      return
    }
    for (const e of entries) {
      const full = join(dir, e.name)
      let st
      try {
        st = statSync(full)
      } catch {
        continue
      }
      const rel = relative(base, full).split('\\').join('/')
      if (e.isDirectory()) {
        rows.push([rel, 0, Math.round(st.mtimeMs), 1])
        walk(full, depth + 1)
      } else if (e.isFile()) {
        rows.push([rel, st.size, Math.round(st.mtimeMs), 0])
      }
    }
  }
  walk(base, 0)
  db.exec('BEGIN')
  try {
    q.indexClear.run(session)
    for (const r of rows) q.indexPut.run(session, ...r)
    q.indexMetaPut.run(session, Date.now(), rows.filter((r) => r[3] === 0).length)
    db.exec('COMMIT')
  } catch (e) {
    db.exec('ROLLBACK')
    throw e
  }
}

function indexView(session) {
  const meta = q.indexMeta.get(session)
  const files = q.indexFiles.all(session).map((r) => ({ path: r.path, size: r.size, mtime: r.mtime, is_dir: r.is_dir === 1 }))
  return { generation: meta?.generation ?? 0, last_scan: meta?.last_scan ?? null, files }
}

// ---------------------------------------------------------------- the site grabber over the fixture sites

function wildcard(p) {
  return new RegExp(`^${p.replace(/[.+^${}()|[\]\\]/g, '\\$&').replace(/\*/g, '.*').replace(/\?/g, '.')}$`, 'i')
}

function matchesAny(patterns, path) {
  return patterns.some((p) => {
    const t = String(p).trim()
    if (!t) return false
    return /[*?]/.test(t) ? wildcard(t).test(path) : path.toLowerCase().includes(t.toLowerCase())
  })
}

function extOf(name) {
  const n = name.toLowerCase()
  if (n.endsWith('.tar.gz')) return 'gz'
  const i = n.lastIndexOf('.')
  return i > 0 ? n.slice(i + 1) : ''
}

async function fetchPage(url) {
  const ctl = new AbortController()
  const t = setTimeout(() => ctl.abort(), 5000)
  try {
    const r = await fetch(url, { signal: ctl.signal, redirect: 'manual' })
    if (!r.ok || !(r.headers.get('content-type') ?? '').includes('text/html')) return null
    const text = await r.text()
    return text.slice(0, 512 * 1024)
  } catch {
    return null
  } finally {
    clearTimeout(t)
  }
}

async function sitePreview(req) {
  const empty = (error) => ({ ok: false, error, host: '', pages: [], files: [], skipped: [], totalBytes: 0 })
  let start
  try {
    start = new URL(String(req.startUrl ?? ''))
  } catch {
    return empty('The start page is not an address.')
  }
  if (start.protocol !== 'https:' && start.protocol !== 'http:') return empty('The start page must be an http or https address.')
  if (start.username || start.password) return empty('No user name or password in the address.')
  if (!fixtureHosts.has(start.hostname.toLowerCase())) {
    return empty(`The hosted pre-alpha walks only its sample sites (${[...fixtureHosts].join(', ')}), never the internet. Install Zeus to grab any site.`)
  }
  const depthMax = Math.max(0, Math.min(4, Number(req.depth) || 0))
  const maxFiles = Math.max(1, Math.min(200, Number(req.maxFiles) || 50))
  const maxBytes = Math.max(1, Math.min(4096, Number(req.maxMiB) || 500)) * 1024 * 1024
  const types = Array.isArray(req.types) ? req.types.map(String) : []
  const include = Array.isArray(req.include) ? req.include : []
  const exclude = Array.isArray(req.exclude) ? req.exclude : []
  const pages = []
  const files = []
  const skipped = new Map()
  const skip = (reason) => skipped.set(reason, (skipped.get(reason) ?? 0) + 1)
  const seen = new Set()
  let totalBytes = 0
  let frontier = [{ url: `${start.origin}${start.pathname}`, depth: 0 }]
  let fetched = 0
  while (frontier.length && fetched < 60) {
    const next = []
    for (const page of frontier) {
      if (seen.has(page.url)) continue
      seen.add(page.url)
      const html = await fetchPage(page.url)
      fetched++
      if (html === null) {
        skip('page did not answer')
        continue
      }
      pages.push(page)
      for (const m of html.matchAll(/href="([^"#]+)"/gi)) {
        let u
        try {
          u = new URL(m[1], page.url)
        } catch {
          continue
        }
        if (u.search) u.search = ''
        const sameHost = u.hostname === start.hostname
        if (!fixtureHosts.has(u.hostname.toLowerCase())) {
          skip('link to a host outside the sample sites')
          continue
        }
        const isPage = u.pathname.endsWith('/') || u.pathname.endsWith('.html')
        if (isPage) {
          if (!sameHost && req.sameHostOnly) {
            skip('link to another host')
            continue
          }
          if (page.depth + 1 > depthMax) continue
          if (matchesAny(exclude, u.pathname)) {
            skip('page excluded by a rule')
            continue
          }
          if (!seen.has(u.href)) next.push({ url: u.href, depth: page.depth + 1 })
          continue
        }
        if (!sameHost && req.sameHostOnly) {
          skip('file on another host')
          continue
        }
        const name = decodeURIComponent(u.pathname.split('/').pop() ?? '')
        const e = extOf(name)
        if (types.length && !types.includes(e)) {
          skip('file type not selected')
          continue
        }
        if (include.some((p) => String(p).trim()) && !matchesAny(include, u.pathname)) {
          skip('not matched by an include rule')
          continue
        }
        if (matchesAny(exclude, u.pathname)) {
          skip('file excluded by a rule')
          continue
        }
        if (files.some((f) => f.url === u.href)) {
          skip('duplicate')
          continue
        }
        if (files.length >= maxFiles) {
          skip('over the file limit')
          continue
        }
        const size = catalogue.sizes?.[`${u.hostname.toLowerCase()}${u.pathname}`] ?? 0
        if (totalBytes + size > maxBytes) {
          skip('over the size limit')
          continue
        }
        totalBytes += size
        files.push({ url: u.href, name, size, depth: page.depth, from: page.url, host: u.hostname })
      }
    }
    frontier = next
  }
  return { ok: true, host: start.hostname, pages, files, skipped: [...skipped.entries()].map(([reason, count]) => ({ reason, count })), totalBytes }
}

/** The link grabber's online check: a HEAD request, to the sample hosts only. */
async function checkLink(raw) {
  let u
  try {
    u = new URL(raw)
  } catch {
    return { url: raw, check: 'offline', size: null, status: 0 }
  }
  if (!fixtureHosts.has(u.hostname.toLowerCase()) || !['http:', 'https:'].includes(u.protocol)) return { url: raw, check: 'offline', size: null, status: 0 }
  const ctl = new AbortController()
  const t = setTimeout(() => ctl.abort(), 4000)
  try {
    const r = await fetch(u, { method: 'HEAD', signal: ctl.signal, redirect: 'manual' })
    const len = Number(r.headers.get('content-length'))
    return { url: raw, check: r.ok ? 'online' : r.status === 401 ? 'needs_login' : 'offline', size: r.ok && Number.isFinite(len) ? len : null, status: r.status }
  } catch {
    return { url: raw, check: 'offline', size: null, status: 0 }
  } finally {
    clearTimeout(t)
  }
}

// ---------------------------------------------------------------- approvals held by this service

const HELD_KINDS = new Set(['workflow_plan', 'destructive_op'])

function validApproval(a) {
  if (!a || typeof a !== 'object' || typeof a.id !== 'string' || !/^ap_[0-9A-Za-z]{10,40}$/.test(a.id)) return 'bad id'
  if (!a.facts || typeof a.facts !== 'object' || !HELD_KINDS.has(a.facts.kind)) return 'this service holds only plan and file-move requests'
  let hash
  try {
    hash = sha256(canonical(a.facts))
  } catch {
    return 'facts are not canonical'
  }
  if (hash !== a.requestHash) return 'the request hash does not match the facts'
  return null
}

function decide(session, id, d) {
  const row = q.approval.get(session, id)
  if (!row) return { status: 404, error: ['approval.not_found', 'No such request.'] }
  const a = JSON.parse(row.json)
  if (a.state !== 'pending') return { status: 409, error: ['approval.not_pending', `This request is already ${a.state}.`] }
  if (Date.parse(a.facts.expires_at) <= Date.now()) {
    a.state = 'expired'
    q.putApproval.run(session, id, JSON.stringify(a), Date.now())
    return { status: 409, error: ['approval.expired', 'This request expired.'] }
  }
  if (d.seen_hash !== a.requestHash) return { status: 409, error: ['approval.hash_mismatch', `The request changed after you read it (now revision ${a.facts.revision}). Read the facts again.`] }
  if (d.decision !== 'approve' && d.decision !== 'deny') return { status: 400, error: ['api.invalid_params', 'Approve or deny.'] }
  if (d.decision === 'approve' && !(Number(d.armed_ms) >= ARMING_MS)) return { status: 409, error: ['approval.not_armed', 'The approve button was pressed before it armed.'] }
  const scopes = a.facts.allowed_scopes ?? []
  if (d.decision === 'approve' && scopes.length && !scopes.includes(d.scope)) return { status: 400, error: ['approval.scope', 'Choose one of the offered scopes.'] }
  a.state = d.decision === 'approve' && d.scope !== 'never' ? 'approved' : 'denied'
  a.decided = { at: Date.now(), decision: d.decision, ...(d.scope ? { scope: d.scope } : {}), via: 'Pre-alpha service (this page)' }
  q.putApproval.run(session, id, JSON.stringify(a), Date.now())
  return { status: 200, approval: a }
}

// ---------------------------------------------------------------- routes

async function handle(req, res) {
  const url = new URL(req.url ?? '/', 'http://svc.internal')
  const path = url.pathname
  if (path === '/healthz') return send(res, 200, { ok: true })
  const session = String(req.headers['x-zeus-session'] ?? '')
  if (!/^[0-9a-f]{32}$/.test(session)) return fail(res, 401, 'svc.session_required', 'Start a session first.')
  q.touch.run(session, Date.now())
  if (Date.now() - (q.firstSeen.get(session)?.first_seen ?? Date.now()) > TTL_MS) return fail(res, 401, 'svc.session_expired', 'This sandbox expired. Reload to start a new one.')

  const m = path.match(/^\/svc\/v1\/(.*)$/)
  if (!m) return fail(res, 404, 'svc.not_found', 'Unknown endpoint.')
  const route = m[1]

  if (route === 'state' && req.method === 'GET') {
    const slices = {}
    for (const r of q.slices.all(session)) slices[r.slice] = { value: JSON.parse(r.json), updated: r.updated }
    return send(res, 200, { slices, first_seen: q.firstSeen.get(session)?.first_seen ?? null, ttl_ms: TTL_MS })
  }
  if (route === 'state' && req.method === 'DELETE') {
    q.clear.run(session)
    return send(res, 200, { ok: true })
  }
  const slice = route.match(/^state\/([a-z]+)$/)?.[1]
  if (slice && req.method === 'PUT') {
    if (!SLICES.has(slice)) return fail(res, 400, 'svc.unknown_slice', 'That setting is not stored here.')
    const value = await body(req)
    const text = JSON.stringify(value)
    if (looksSecret(text)) return fail(res, 400, 'svc.secret_refused', 'Provider keys are never stored on the server; they stay in your browser memory.')
    const others = q.size.get(session, slice)?.n ?? 0
    if (others + text.length > MAX_SESSION_BYTES) return fail(res, 413, 'svc.quota', 'This sandbox stores at most 4 MiB of settings and lists.')
    q.put.run(session, slice, text, Date.now())
    return send(res, 200, { ok: true, updated: Date.now() })
  }

  if (route === 'index' && req.method === 'GET') {
    if (!q.indexMeta.get(session)) scan(session)
    return send(res, 200, indexView(session))
  }
  if (route === 'index/rescan' && req.method === 'POST') {
    scan(session)
    return send(res, 200, indexView(session))
  }

  if (route === 'links/check' && req.method === 'POST') {
    const b = (await body(req, 64 * 1024)) ?? {}
    const urls = Array.isArray(b.urls) ? b.urls.slice(0, 50).map(String) : []
    return send(res, 200, { items: await Promise.all(urls.map(checkLink)) })
  }

  if (route === 'sitegrab/preview' && req.method === 'POST') {
    return send(res, 200, await sitePreview((await body(req, 64 * 1024)) ?? {}))
  }

  if (route === 'approvals' && req.method === 'GET') {
    return send(res, 200, { items: q.approvals.all(session).map((r) => JSON.parse(r.json)) })
  }
  if (route === 'approvals' && req.method === 'POST') {
    const a = await body(req, 128 * 1024)
    const problem = validApproval(a)
    if (problem) return fail(res, 400, 'svc.bad_approval', problem)
    if ((q.countApprovals.get(session)?.n ?? 0) >= 200) return fail(res, 413, 'svc.quota', 'Too many requests in this sandbox.')
    const existing = q.approval.get(session, a.id)
    if (existing && JSON.parse(existing.json).state !== 'pending') return fail(res, 409, 'approval.not_pending', 'That request was already decided.')
    const stored = { id: a.id, state: 'pending', facts: a.facts, requestHash: a.requestHash, bindCode: String(a.bindCode ?? ''), jobIds: Array.isArray(a.jobIds) ? a.jobIds.slice(0, 200) : [], ...(typeof a.agentSays === 'string' ? { agentSays: a.agentSays.slice(0, 2000) } : {}), heldBy: 'service' }
    q.putApproval.run(session, a.id, JSON.stringify(stored), Date.now())
    return send(res, 200, stored)
  }
  const dec = route.match(/^approvals\/(ap_[0-9A-Za-z]+)\/decide$/)
  if (dec && req.method === 'POST') {
    const r = decide(session, dec[1], (await body(req, 8 * 1024)) ?? {})
    if (r.error) return fail(res, r.status, r.error[0], r.error[1])
    return send(res, 200, r.approval)
  }

  return fail(res, 404, 'svc.not_found', 'Unknown endpoint.')
}

createServer((req, res) => {
  handle(req, res).catch((e) => {
    const status = e?.status ?? 500
    if (!res.headersSent) fail(res, status, status === 413 ? 'svc.too_large' : status === 400 ? 'svc.bad_request' : 'svc.internal', status === 500 ? 'The service could not complete this request.' : String(e.message))
  })
}).listen(PORT, '0.0.0.0', () => {
  console.log(`zeus pre-alpha service on :${PORT}, ${fixtureHosts.size} fixture hosts`)
})
