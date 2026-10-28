import test from 'node:test'
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { createHash, generateKeyPairSync } from 'node:crypto'
import { mkdtempSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { exportJWK, SignJWT } from 'jose'
import { validateConfig } from '../src/config.mjs'
import { createTeamService } from '../src/server.mjs'
import { Store } from '../src/store.mjs'
import { FEATURES, canonical, sha } from '../src/contracts.mjs'
import { POLICY_TYPE, PROVENANCE_TYPE, verifyEnvelope } from '../src/signatures.mjs'

const listen = server => new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
const enabled = () => Object.fromEntries(FEATURES.map(name => [name, true]))
const pkce = verifier => createHash('sha256').update(verifier).digest('base64url')
async function fixture(t, database = ':memory:') {
  let now = Date.now(), active = 0, lastToken = ''
  const pairs = [generateKeyPairSync('ed25519'), generateKeyPairSync('ed25519')]
  const keys = await Promise.all(pairs.map(async (pair, index) => ({ ...await exportJWK(pair.publicKey), kid: 'oidc_' + index, alg: 'EdDSA' })))
  const codes = new Map()
  let issuer
  const identityServer = createServer(async (req, res) => {
    res.setHeader('content-type', 'application/json')
    const path = new URL(req.url, issuer).pathname
    if (path === '/.well-known/openid-configuration') return res.end(JSON.stringify({ issuer, authorization_endpoint: issuer + '/authorize', token_endpoint: issuer + '/token', jwks_uri: issuer + '/keys', response_types_supported: ['code'], code_challenge_methods_supported: ['S256'], token_endpoint_auth_methods_supported: ['client_secret_basic'] }))
    if (path === '/keys') return res.end(JSON.stringify({ keys: [keys[active]] }))
    if (path !== '/token' || req.method !== 'POST') { res.statusCode = 404; return res.end('{}') }
    let body = ''; for await (const chunk of req) body += chunk
    const form = new URLSearchParams(body), code = codes.get(form.get('code'))
    assert.equal(req.headers.authorization, 'Basic ' + Buffer.from('fixture-client:fixture-secret').toString('base64'))
    assert(code && pkce(form.get('code_verifier')) === code.challenge, 'Provider rejected the PKCE verifier')
    assert.equal(form.get('redirect_uri'), config.origin + '/auth/callback')
    codes.delete(form.get('code'))
    lastToken = await new SignJWT({ nonce: code.nonce, groups: ['writers'] }).setProtectedHeader({ alg: 'EdDSA', kid: keys[active].kid }).setIssuer(issuer).setAudience('fixture-client').setSubject('admin').setIssuedAt(Math.floor(now / 1000)).setExpirationTime(Math.floor(now / 1000) + 3600).sign(pairs[active].privateKey)
    res.end(JSON.stringify({ id_token: lastToken, access_token: 'OIDC-ACCESS-CANARY', refresh_token: 'OIDC-REFRESH-CANARY' }))
  })
  await listen(identityServer); issuer = `http://127.0.0.1:${identityServer.address().port}`
  const signing = generateKeyPairSync('ed25519')
  const team = id => ({ id, label: id, features: enabled(), bootstrap_members: [{ issuer, subject: 'admin', role: 'admin' }, { issuer, subject: 'reader', role: 'reader' }], group_roles: [{ issuer, group: 'writers', role: 'writer' }] })
  const config = validateConfig({ origin: 'http://127.0.0.1:1', issuer, client_id: 'fixture-client', audience: 'fixture-client', client_secret: 'fixture-secret', token_auth_method: 'client_secret_basic', oidc_origins: [issuer], groups_claim: 'groups', database, listen_host: '127.0.0.1', listen_port: 0, teams: [team('alpha'), team('beta')], signing: { active_key: 'team_key', keys: { team_key: signing.privateKey.export({ type: 'pkcs8', format: 'pem' }).toString() } } }, { fixture: true })
  const app = createTeamService(config, { clock: () => now })
  await listen(app.server); config.origin = `http://127.0.0.1:${app.server.address().port}`; app.publisher.issuer = config.origin
  t.after(async () => { await app.close(); await new Promise(resolve => identityServer.close(resolve)) })
  const actor = subject => ({ issuer, subject, groups: [], oidc_expires_at_ms: now + 3600000 })
  const request = async (path, { token, method = 'GET', body, headers = {} } = {}) => {
    const response = await fetch(config.origin + path, { method, headers: { ...(token ? { authorization: 'Bearer ' + token } : {}), ...(body === undefined ? {} : { 'content-type': 'application/json' }), ...headers }, ...(body === undefined ? {} : { body: JSON.stringify(body) }), redirect: 'manual' })
    const value = await response.json(); return { status: response.status, headers: response.headers, value }
  }
  return { app, config, actor, request, token: subject => app.store.createSession('alpha', actor(subject)).access_token, advance: ms => { now += ms }, rotate: () => { active = 1 }, code: (authorization, id = 'fixture-code') => { const url = new URL(authorization); codes.set(id, { nonce: url.searchParams.get('nonce'), challenge: url.searchParams.get('code_challenge') }); return { state: url.searchParams.get('state'), code: id } }, lastToken: () => lastToken }
}

test('OIDC code+PKCE+nonce enrollment is one-use and does not retain provider credentials', async t => {
  const f = await fixture(t), verifier = 'v'.repeat(64)
  const begin = f.app.auth.begin({ alias: 'Fixture desktop', code_challenge: pkce(verifier) })
  const login = await f.app.auth.login(begin.user_code)
  const callback = f.code(login.authorization)
  await assert.rejects(f.app.auth.callback(callback, 'x'.repeat(43)), { code: 'team.invalid_callback' })
  const approval = await f.app.auth.callback(callback, login.cookie)
  assert(approval.html.includes(begin.user_code))
  await assert.rejects(f.app.auth.callback(callback, login.cookie), { code: 'team.invalid_callback' })
  const csrf = /name="csrf" value="([^"]+)"/u.exec(approval.html)[1]
  assert.throws(() => f.app.auth.approve({ csrf: 'x'.repeat(43), tenant_id: 'alpha', decision: 'approve' }, approval.grant), { code: 'team.invalid_approval' })
  f.app.auth.approve({ csrf, tenant_id: 'alpha', decision: 'approve' }, approval.grant)
  f.advance(1001)
  const session = f.app.auth.poll({ device_code: begin.device_code, code_verifier: verifier })
  assert(session.access_token)
  assert.throws(() => f.app.auth.poll({ device_code: begin.device_code, code_verifier: verifier }), { code: 'team.enrollment_already_claimed' })
  const tables = ['devices', 'sessions', 'receipts', 'records', 'audit']
  const rows = JSON.stringify(tables.map(name => f.app.store.db.prepare('SELECT * FROM ' + name).all()))
  for (const secret of ['OIDC-ACCESS-CANARY', 'OIDC-REFRESH-CANARY', f.lastToken(), session.access_token]) assert(!rows.includes(secret))
})

test('OIDC key rotation refreshes pinned JWKS and rejects a wrong nonce', async t => {
  const f = await fixture(t)
  async function signedIn(code) {
    const v = 'a'.repeat(64), device = f.app.auth.begin({ alias: 'Rotation fixture', code_challenge: pkce(v) }), login = await f.app.auth.login(device.user_code)
    return f.app.auth.callback(f.code(login.authorization, code), login.cookie)
  }
  await signedIn('first'); f.advance(2000); f.rotate(); await signedIn('rotated')
  await assert.rejects(f.app.oidc.verify(f.lastToken(), 'wrong-nonce'), { code: 'team.invalid_id_token' })
})

test('HTTP tenant and role boundaries cannot be replaced by caller labels', async t => {
  const f = await fixture(t), admin = f.token('admin'), reader = f.token('reader')
  assert.equal((await f.request('/v1/session')).status, 401)
  assert.equal((await f.request('/v1/teams/beta/entitlements', { token: admin })).status, 404)
  const options = { token: reader, method: 'PUT', body: { label: 'Read only cannot write', color: '#112233' }, headers: { 'if-match': '"0"', 'idempotency-key': 'read-write' } }
  assert.equal((await f.request('/v1/teams/alpha/metadata/tags/tag1', options)).status, 403)
  assert.equal((await f.request('/v1/session', { token: admin, headers: { origin: 'https://evil.invalid' } })).status, 403)
  assert.equal((await f.request('/v1/teams/alpha/entitlements', { token: admin })).status, 200)
})

test('metadata CAS, exact retries, stable snapshots, and tombstones survive store restart', async t => {
  const root = mkdtempSync(join(tmpdir(), 'zeus-team-')), path = join(root, 'metadata.sqlite')
  const f = await fixture(t, path), who = f.actor('admin'), put = (id, revision, label, key) => f.app.store.put('alpha', who, 'tags', id, revision, { label, color: '#112233' }, false, key)
  const first = put('one', 0, 'One', 'create-one'); assert.equal(first.revision, 1)
  assert.deepEqual(put('one', 0, 'One', 'create-one'), first)
  assert.throws(() => put('one', 0, 'Changed', 'create-one'), { code: 'team.idempotency_conflict' })
  assert.throws(() => put('one', 0, 'Changed', 'stale'), { code: 'team.revision_conflict' })
  put('two', 0, 'Two', 'create-two'); const snapshot = f.app.store.team('alpha').seq
  put('three', 0, 'Three', 'create-three')
  const page = f.app.store.pull('alpha', who, 0, snapshot, 1); assert(page.more); assert.equal(page.records[0].record_id, 'one')
  const rest = f.app.store.pull('alpha', who, page.records[0].sequence, snapshot, 100); assert.deepEqual(rest.records.map(row => row.record_id), ['two'])
  f.app.store.put('alpha', who, 'tags', 'one', 1, null, true, 'delete-one')
  const reopened = new Store(path, f.config.teams); t.after(() => { reopened.close() })
  const records = reopened.pull('alpha', who, 0, reopened.team('alpha').seq, 100).records
  assert.equal(records.at(-1).tombstone, true); assert.equal(records.at(-1).value, null)
  assert.equal(reopened.receipt('alpha', who, 'metadata.put', 'create-one').revision, 1)
})

test('session rotation and revocation immediately reject the previous bearer', async t => {
  const f = await fixture(t), token = f.token('admin')
  const rotated = await f.request('/v1/session/rotate', { method: 'POST', token }); assert.equal(rotated.status, 200)
  assert.equal((await f.request('/v1/session', { token })).status, 401)
  const next = rotated.value.access_token; assert.equal((await f.request('/v1/session', { token: next })).status, 200)
  assert.equal((await f.request('/v1/session/revoke', { method: 'POST', token: next })).status, 200)
  assert.equal((await f.request('/v1/session', { token: next })).status, 401)
})

test('signed policy and client-reported provenance use separate pinned DSSE domains', async t => {
  const f = await fixture(t), token = f.token('admin')
  const policy = await f.request('/v1/teams/alpha/policy', { token }); assert.equal(policy.status, 200)
  const keys = await f.request('/v1/teams/alpha/keys', { token }), pins = Object.fromEntries(keys.value.keys.map(key => [key.key_id, key.spki]))
  assert.equal(verifyEnvelope(policy.value, POLICY_TYPE, pins).tenant_id, 'alpha')
  assert.throws(() => verifyEnvelope(policy.value, PROVENANCE_TYPE, pins))
  const report = await f.request('/v1/teams/alpha/provenance', { method: 'POST', token, body: { sha256: 'a'.repeat(64), size_bytes: 123, source_kind: 'http' }, headers: { 'idempotency-key': 'provenance-one' } }); assert.equal(report.status, 200)
  const signed = verifyEnvelope(report.value, PROVENANCE_TYPE, pins); assert.equal(signed.assertion_kind, 'client_reported')
  const tampered = { ...report.value, payload: Buffer.from('{}').toString('base64') }; assert.throws(() => verifyEnvelope(tampered, PROVENANCE_TYPE, pins))
  const audit = await f.request('/v1/teams/alpha/audit', { token }); assert.equal(audit.status, 200)
  let previous = ''; for (const row of audit.value.entries) { assert.equal(row.previous_hash, previous); assert.equal(row.hash, sha(previous + '\0' + canonical(row.entry))); previous = row.hash }
})

test('metadata rejects secret-bearing fields and disabled sync before mutation', async t => {
  const f = await fixture(t), admin = f.actor('admin')
  for (const value of [{ label: 'Tag', color: '#112233', url: 'https://secret.invalid/?token=CANARY' }, { label: 'Tag', color: '#112233', cookie: 'CANARY' }]) assert.throws(() => f.app.store.put('alpha', admin, 'tags', 'bad', 0, value, false, 'reject-' + Object.keys(value).at(-1)))
  const features = { ...enabled(), metadata_sync: false }
  f.app.store.setFeatures('alpha', admin, 1, features, 'disable-sync')
  assert.throws(() => f.app.store.put('alpha', admin, 'tags', 'tag', 0, { label: 'Tag', color: '#112233' }, false, 'after-disable'), { code: 'team.feature_disabled' })
  assert.equal(f.app.store.team('alpha').seq, 0)
})
