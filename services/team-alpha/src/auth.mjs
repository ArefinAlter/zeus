import { createHash, randomBytes } from 'node:crypto'
import { escapeHtml, fail, id, integer, label, object, random, same, sha, text } from './contracts.mjs'

const challenge = verifier => createHash('sha256').update(verifier).digest('base64url')
const secret = value => text(value, 128, /^[A-Za-z0-9_.~-]{43,128}$/u)

export class Auth {
  constructor(config, store, oidc, clock = Date.now) { this.config = config; this.store = store; this.oidc = oidc; this.clock = clock; this.flows = new Map(); this.grants = new Map() }
  prune() {
    for (const map of [this.flows, this.grants]) for (const [key, value] of map) if (value.expires <= this.clock()) map.delete(key)
    this.store.db.prepare('DELETE FROM devices WHERE expires<=?').run(this.clock())
  }
  begin(input) {
    object(input, ['alias', 'code_challenge']); label(input.alias); secret(input.code_challenge)
    if (input.code_challenge.length !== 43) fail('team.invalid_device_challenge')
    this.prune()
    if (this.store.db.prepare('SELECT COUNT(*) AS n FROM devices').get().n >= 128) fail('team.auth_capacity', 429)
    const device = random(); const raw = randomBytes(5).toString('hex').toUpperCase(); const code = raw.slice(0, 5) + '-' + raw.slice(5)
    const expires = this.clock() + 600000
    this.store.db.prepare('INSERT INTO devices(hash,user_hash,id,alias,challenge,created,expires,state,next_poll) VALUES (?,?,?,?,?,?,?,?,?)').run(sha(device), sha(code), 'device_' + random(16), input.alias, input.code_challenge, this.clock(), expires, 'waiting', this.clock() + 1000)
    return { device_code: device, user_code: code, verification_uri: this.config.origin + '/auth/authorize?user_code=' + code, expires_in_ms: 600000, poll_interval_ms: 1000 }
  }
  deviceByCode(code) {
    text(code, 11, /^[A-F0-9]{5}-[A-F0-9]{5}$/u)
    const device = this.store.db.prepare('SELECT * FROM devices WHERE user_hash=? AND expires>? AND state=\'waiting\'').get(sha(code), this.clock())
    return device ?? fail('team.enrollment_unavailable', 404)
  }
  async login(code) {
    this.prune(); this.deviceByCode(code)
    if (this.flows.size >= 128) fail('team.auth_capacity', 429)
    const state = random(); const cookie = random(); const verifier = random()
    const flow = { state, user_code: code, cookie_hash: sha(cookie), verifier, challenge: challenge(verifier), nonce: random(), expires: this.clock() + 600000 }
    const authorization = await this.oidc.authorization(flow)
    // Publish only after bounded discovery succeeds; no callback can consume an incomplete flow.
    this.flows.set(sha(state), flow)
    return { authorization, cookie }
  }
  async callback(input, cookie) {
    object(input, ['state', 'code']); secret(input.state); text(input.code, 8192); secret(cookie)
    this.prune()
    const key = sha(input.state); const flow = this.flows.get(key)
    if (!flow || !same(flow.cookie_hash, sha(cookie))) fail('team.invalid_callback', 401)
    // Consume before a one-use code exchange. A lost response requires a fresh sign-in.
    this.flows.delete(key)
    this.deviceByCode(flow.user_code)
    const identity = await this.oidc.redeem(input.code, flow)
    const teams = this.store.teamsFor(identity)
    if (!teams.length) fail('team.membership_required', 403)
    if (this.grants.size >= 128) fail('team.auth_capacity', 429)
    const grant = random(); const csrf = random()
    this.grants.set(sha(grant), { identity, user_code: flow.user_code, csrf: sha(csrf), expires: this.clock() + 300000 })
    const device = this.deviceByCode(flow.user_code)
    return { grant, html: `<!doctype html><meta charset="utf-8"><meta name="viewport" content="width=device-width"><title>Connect Zeus desktop</title><main><h1>Connect Zeus desktop</h1><p>Compare this code with your terminal: <strong>${escapeHtml(flow.user_code)}</strong></p><p>Device label: ${escapeHtml(device.alias)}</p><p>Identity provider: ${escapeHtml(identity.issuer)}</p><p>This connection shares only metadata you explicitly select. It cannot approve downloads, uploads or access browser logins.</p><form method="post" action="/auth/approve"><input type="hidden" name="csrf" value="${csrf}"><label>Team <select name="tenant_id">${teams.map(team => `<option value="${escapeHtml(team.tenant_id)}">${escapeHtml(team.label)} (${team.role})</option>`).join('')}</select></label><button name="decision" value="approve">Connect this desktop</button><button name="decision" value="deny">Decline</button></form></main>` }
  }
  approve(input, cookie) {
    object(input, ['csrf', 'tenant_id', 'decision']); secret(input.csrf); secret(cookie); id(input.tenant_id)
    if (!['approve', 'deny'].includes(input.decision)) fail('team.invalid_decision')
    this.prune()
    const key = sha(cookie); const grant = this.grants.get(key)
    if (!grant || !same(grant.csrf, sha(input.csrf))) fail('team.invalid_approval', 401)
    this.grants.delete(key)
    const device = this.deviceByCode(grant.user_code)
    this.store.admit(input.tenant_id, grant.identity)
    this.store.transaction(() => {
      const current = this.deviceByCode(grant.user_code)
      if (current.hash !== device.hash) fail('team.enrollment_unavailable', 409)
      this.store.db.prepare('UPDATE devices SET state=?,approved=?,team=? WHERE hash=? AND state=\'waiting\'').run(input.decision === 'approve' ? 'approved' : 'denied', JSON.stringify(grant.identity), input.tenant_id, device.hash)
      this.store.audit(input.tenant_id, grant.identity, 'device.' + input.decision, { device_id: device.id })
    })
    return { approved: input.decision === 'approve' }
  }
  poll(input) {
    object(input, ['device_code', 'code_verifier']); secret(input.device_code); secret(input.code_verifier)
    this.prune()
    return this.store.transaction(() => {
      const device = this.store.db.prepare('SELECT * FROM devices WHERE hash=? AND expires>?').get(sha(input.device_code), this.clock())
      if (!device || !same(challenge(input.code_verifier), device.challenge)) fail('team.enrollment_unavailable', 401)
      if (device.state === 'consumed') fail('team.enrollment_already_claimed', 409)
      if (device.state === 'denied') fail('team.enrollment_denied', 403)
      if (this.clock() < device.next_poll) return { state: 'waiting', poll_after_ms: device.next_poll - this.clock() }
      this.store.db.prepare('UPDATE devices SET next_poll=? WHERE hash=?').run(this.clock() + 1000, device.hash)
      if (device.state === 'waiting') return { state: 'waiting', poll_after_ms: 1000 }
      const identity = JSON.parse(device.approved)
      const session = this.store.createSession(device.team, identity)
      this.store.db.prepare('UPDATE devices SET state=\'consumed\',approved=NULL WHERE hash=?').run(device.hash)
      this.store.audit(device.team, identity, 'device.claim', { device_id: device.id })
      return { state: 'connected', ...session }
    })
  }
  cancel(input) {
    object(input, ['device_code', 'code_verifier']); secret(input.device_code); secret(input.code_verifier)
    const device = this.store.db.prepare('SELECT * FROM devices WHERE hash=?').get(sha(input.device_code))
    if (!device || !same(challenge(input.code_verifier), device.challenge)) fail('team.enrollment_unavailable', 401)
    if (device.state === 'consumed') fail('team.enrollment_already_claimed', 409)
    this.store.db.prepare('DELETE FROM devices WHERE hash=?').run(device.hash)
    return { canceled: true }
  }
}
