import { createServer } from 'node:http'
import { pathToFileURL } from 'node:url'
import { Auth } from './auth.mjs'
import { loadConfig, runtime } from './config.mjs'
import { actorId, canonical, escapeHtml, fail, Failure, id, integer, object, provenance, random, sha, text, utc } from './contracts.mjs'
import { Oidc } from './oidc.mjs'
import { Publisher } from './signatures.mjs'
import { Store } from './store.mjs'

const SECURITY = { 'cache-control': 'no-store', 'x-content-type-options': 'nosniff', 'referrer-policy': 'no-referrer', 'content-security-policy': "default-src 'none'; form-action 'self'; base-uri 'none'; frame-ancestors 'none'", 'x-frame-options': 'DENY' }
function send(res, status, value, headers = {}) {
  const bytes = Buffer.from(typeof value === 'string' ? value : canonical(value))
  res.writeHead(status, { ...SECURITY, 'content-type': typeof value === 'string' ? 'text/html; charset=utf-8' : 'application/json', 'content-length': bytes.length, ...headers }); res.end(bytes)
}
function cookie(req, name) {
  const matches = String(req.headers.cookie ?? '').split(';').map(part => part.trim().split('=')).filter(([key]) => key === name)
  if (matches.length !== 1 || matches[0].length !== 2) fail('team.invalid_cookie', 401)
  return matches[0][1]
}
function setCookie(config, name, value, sameSite) { return `${name}=${value}; Path=/auth/; HttpOnly; SameSite=${sameSite}; Max-Age=600${config.origin.startsWith('https:') ? '; Secure' : ''}` }
async function body(req, form = false) {
  const expected = form ? 'application/x-www-form-urlencoded' : 'application/json'
  if (!String(req.headers['content-type'] ?? '').startsWith(expected)) fail('team.content_type_required', 415)
  const parts = []; let count = 0
  for await (const part of req) { count += part.length; if (count > 65536) fail('team.payload_too_large', 413); parts.push(part) }
  try {
    const raw = Buffer.concat(parts).toString('utf8')
    if (!form) return JSON.parse(raw)
    const values = new URLSearchParams(raw); const keys = [...values.keys()]
    if (new Set(keys).size !== keys.length) fail('team.duplicate_form_field')
    return Object.fromEntries(values)
  } catch (error) { if (error instanceof Failure) throw error; fail('team.invalid_json') }
}
function query(url, allowed) {
  for (const key of url.searchParams.keys()) if (!allowed.includes(key) || url.searchParams.getAll(key).length !== 1) fail('team.invalid_query')
}
function bearer(req) {
  const match = /^Bearer ([A-Za-z0-9_-]{43})$/u.exec(req.headers.authorization ?? '')
  return match?.[1] ?? fail('team.unauthorized', 401)
}
function revision(req) {
  const value = req.headers['if-match']
  const match = /^"([0-9]+)"$/u.exec(value ?? '')
  if (!match) fail('team.revision_required', 428)
  return integer(Number(match[1]))
}
export function createTeamService(config, { clock = Date.now } = {}) {
  runtime()
  const store = new Store(config.database, config.teams, clock)
  const oidc = new Oidc(config, clock); const auth = new Auth(config, store, oidc, clock)
  const publisher = new Publisher({ issuer: config.origin, activeKey: config.signing.active_key, keys: config.signing.keys })
  const cursors = new Map(); const rates = new Map()
  const prune = () => {
    auth.prune()
    for (const [key, cursor] of cursors) if (cursor.expires <= clock()) cursors.delete(key)
    for (const [key, rate] of rates) if (rate.until <= clock()) rates.delete(key)
  }
  const timer = setInterval(prune, 60000); timer.unref()
  const server = createServer({ maxHeaderSize: 16384, requestTimeout: 15000, headersTimeout: 5000 }, async (req, res) => {
    try {
      if (req.headers.host !== new URL(config.origin).host) fail('team.invalid_host', 421)
      const url = new URL(req.url, config.origin)
      if (url.origin !== config.origin || url.hash) fail('team.invalid_target')
      if (req.headers.origin && req.headers.origin !== config.origin && url.pathname !== '/auth/callback') fail('team.cross_origin', 403)
      prune()
      const address = req.socket.remoteAddress ?? 'unknown'
      const rateKey = sha(address + '\0' + (url.pathname.startsWith('/auth/') ? 'auth' : String(req.headers.authorization ?? '').slice(0, 64)))
      const rate = rates.get(rateKey) ?? { until: clock() + 60000, count: 0 }
      if (++rate.count > (url.pathname.startsWith('/auth/') ? 120 : 600)) fail('team.rate_limited', 429)
      if (!rates.has(rateKey) && rates.size >= 10000) fail('team.rate_capacity', 429)
      rates.set(rateKey, rate)
      if (req.method === 'GET' && url.pathname === '/health') return send(res, 200, { service: 'zeus-team-alpha', version: 1 })
      if (req.method === 'POST' && url.pathname === '/auth/device') return send(res, 200, auth.begin(await body(req)))
      if (req.method === 'POST' && url.pathname === '/auth/device/poll') {
        const value = auth.poll(await body(req))
        if (value.expires_at_ms !== undefined) { value.expires_at = utc(value.expires_at_ms); delete value.expires_at_ms }
        return send(res, 200, value)
      }
      if (req.method === 'POST' && url.pathname === '/auth/device/cancel') return send(res, 200, auth.cancel(await body(req)))
      if (req.method === 'GET' && url.pathname === '/auth/authorize') {
        query(url, ['user_code']); const login = await auth.login(url.searchParams.get('user_code'))
        return send(res, 303, '', { location: login.authorization, 'set-cookie': setCookie(config, 'zt_flow', login.cookie, 'Lax') })
      }
      if (req.method === 'GET' && url.pathname === '/auth/callback') {
        query(url, ['state', 'code']); const grant = await auth.callback(Object.fromEntries(url.searchParams), cookie(req, 'zt_flow'))
        return send(res, 200, grant.html, { 'set-cookie': setCookie(config, 'zt_grant', grant.grant, 'Strict') })
      }
      if (req.method === 'POST' && url.pathname === '/auth/approve') {
        if (req.headers.origin !== config.origin) fail('team.origin_required', 403)
        const decision = auth.approve(await body(req, true), cookie(req, 'zt_grant'))
        return send(res, 200, '<!doctype html><meta charset="utf-8"><title>Zeus connection</title><p>' + (decision.approved ? 'Connection approved. Return to the terminal.' : 'Connection declined.') + '</p>', { 'set-cookie': setCookie(config, 'zt_grant', '', 'Strict') + '; Max-Age=0' })
      }
      const token = bearer(req); const session = store.session(token)
      if (req.method === 'POST' && url.pathname === '/v1/session/rotate') { const value = store.rotateSession(token); value.expires_at = utc(value.expires_at_ms); delete value.expires_at_ms; return send(res, 200, value) }
      if (req.method === 'POST' && url.pathname === '/v1/session/revoke') return send(res, 200, store.revokeSession(token))
      if (req.method === 'GET' && url.pathname === '/v1/session') return send(res, 200, { tenant_id: session.tenant, actor_id: actorId(session.identity), role: store.role(session.tenant, session.identity), expires_at: utc(session.expires_at_ms) })
      const parts = url.pathname.split('/').filter(Boolean)
      if (parts[0] !== 'v1' || parts[1] !== 'teams' || parts[2] !== session.tenant) fail('team.not_found', 404)
      const team = session.tenant; const actor = session.identity
      if (parts.length === 4 && parts[3] === 'policy' && req.method === 'GET') return send(res, 200, store.policy(team, actor, publisher))
      if (parts.length === 4 && parts[3] === 'keys' && req.method === 'GET') return send(res, 200, { issuer: config.origin, keys: publisher.publicKeys() })
      if (parts.length === 4 && parts[3] === 'entitlements' && req.method === 'GET') { store.admit(team, actor); const row = store.team(team); return send(res, 200, { revision: row.revision, features: JSON.parse(row.features) }, { etag: `"${row.revision}"` }) }
      if (parts.length === 4 && parts[3] === 'entitlements' && req.method === 'PUT') {
        const input = await body(req); object(input, ['features'])
        return send(res, 200, store.setFeatures(team, actor, revision(req), input.features, req.headers['idempotency-key']))
      }
      if (parts.length === 4 && parts[3] === 'members' && req.method === 'PUT') {
        const input = await body(req); object(input, ['issuer', 'subject', 'role', 'revoked']); text(input.subject, 255)
        if (input.issuer !== config.issuer || !['reader', 'writer', 'admin'].includes(input.role) || typeof input.revoked !== 'boolean') fail('team.invalid_membership')
        return send(res, 200, store.setMember(team, actor, input, req.headers['idempotency-key']))
      }
      if (parts.length === 6 && parts[3] === 'metadata' && ['PUT', 'DELETE'].includes(req.method)) {
        const value = req.method === 'DELETE' ? null : await body(req)
        const output = store.put(team, actor, parts[4], parts[5], revision(req), value, req.method === 'DELETE', req.headers['idempotency-key'])
        return send(res, 200, output, { etag: `"${output.revision}"` })
      }
      if (parts.length === 4 && parts[3] === 'changes' && req.method === 'GET') {
        query(url, ['after', 'limit', 'cursor'])
        let after; let snapshot; let limit
        const rawCursor = url.searchParams.get('cursor')
        if (rawCursor) {
          if (url.searchParams.size !== 1) fail('team.invalid_cursor')
          const cursor = cursors.get(sha(rawCursor))
          if (!cursor || cursor.tenant !== team || cursor.actor !== actorId(actor) || cursor.expires <= clock()) fail('team.cursor_expired', 410)
          ;({ after, snapshot, limit } = cursor)
        } else { after = integer(Number(url.searchParams.get('after') ?? 0)); limit = integer(Number(url.searchParams.get('limit') ?? 100), 1, 100); snapshot = store.team(team).seq }
        const output = store.pull(team, actor, after, snapshot, limit)
        output.next_cursor = null
        if (output.more) {
          if (cursors.size >= 4096) fail('team.cursor_capacity', 429)
          const next = random(); cursors.set(sha(next), { tenant: team, actor: actorId(actor), after: output.records.at(-1).sequence, snapshot, limit, expires: clock() + 300000 }); output.next_cursor = next
        }
        return send(res, 200, output)
      }
      if (parts.length === 4 && parts[3] === 'provenance' && req.method === 'POST') {
        store.admit(team, actor, 'writer', 'provenance'); const input = provenance(await body(req))
        const output = store.mutate(team, actor, 'provenance.mint', req.headers['idempotency-key'], input, () => { const envelope = publisher.provenance(team, actor, input, clock()); store.audit(team, actor, 'provenance.mint', { sha256: input.sha256, size_bytes: input.size_bytes, assertion_kind: 'client_reported' }); return envelope })
        return send(res, 200, output)
      }
      if (parts.length === 4 && parts[3] === 'receipt' && req.method === 'GET') {
        query(url, ['method', 'key']); return send(res, 200, store.receipt(team, actor, url.searchParams.get('method'), url.searchParams.get('key')))
      }
      if (parts.length === 4 && parts[3] === 'audit' && req.method === 'GET') {
        query(url, ['after', 'limit']); return send(res, 200, { entries: store.auditView(team, actor, integer(Number(url.searchParams.get('after') ?? 0)), integer(Number(url.searchParams.get('limit') ?? 100), 1, 100)) })
      }
      fail('team.not_found', 404)
    } catch (error) {
      if (!res.headersSent) send(res, error instanceof Failure ? error.status : 500, { error: { code: error instanceof Failure ? error.code : 'team.internal', message: 'The team request could not be completed.' } })
      else res.destroy()
    }
  })
  server.maxConnections = 128
  return { server, store, auth, oidc, publisher, close: async () => { clearInterval(timer); await new Promise(resolve => server.close(resolve)); store.close() } }
}
if (process.argv[1] && import.meta.url === pathToFileURL(process.argv[1]).href) {
  try {
    const config = loadConfig(process.env.ZEUS_TEAM_CONFIG)
    const app = createTeamService(config)
    app.server.listen(config.listen_port, config.listen_host)
    for (const signal of ['SIGTERM', 'SIGINT']) process.once(signal, () => { void app.close().then(() => process.exit(0)) })
  } catch { console.error('team.configuration_unavailable'); process.exitCode = 1 }
}
