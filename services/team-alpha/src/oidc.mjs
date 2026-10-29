import { request as httpRequest } from 'node:http'
import { request as httpsRequest } from 'node:https'
import { createLocalJWKSet, decodeProtectedHeader, jwtVerify } from 'jose'
import { fail, integer, same, text } from './contracts.mjs'

// No inherited proxy, redirect, arbitrary JKU or unbounded response is used for issuer traffic.
export function boundedJson(raw, { origins, fixture, method = 'GET', body, headers = {} }) {
  const url = new URL(raw)
  if (!origins.has(url.origin) || url.username || url.password || url.hash || (url.protocol !== 'https:' && !(fixture && url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port))) fail('team.untrusted_oidc_endpoint')
  return new Promise((resolve, reject) => {
    const finishError = () => reject(Object.assign(new Error('team.oidc_transport'), { code: 'team.oidc_transport', status: 502 }))
    const timer = setTimeout(() => { req.destroy(); finishError() }, 15000)
    const req = (url.protocol === 'https:' ? httpsRequest : httpRequest)(url, { method, headers: { accept: 'application/json', ...headers }, agent: false }, res => {
      const parts = []; let size = 0
      const length = Number(res.headers['content-length'])
      if (res.statusCode !== 200 || length > 65536 || !String(res.headers['content-type'] ?? '').toLowerCase().startsWith('application/json')) { res.destroy(); clearTimeout(timer); finishError(); return }
      res.on('data', bytes => { size += bytes.length; if (size > 65536) { res.destroy(); req.destroy(); finishError() } else parts.push(bytes) })
      res.once('error', () => { clearTimeout(timer); finishError() })
      res.once('end', () => {
        clearTimeout(timer)
        try { resolve(JSON.parse(Buffer.concat(parts).toString('utf8'))) } catch { finishError() }
      })
    })
    req.once('error', () => { clearTimeout(timer); finishError() })
    req.end(body)
  })
}
export class Oidc {
  constructor(config, clock = Date.now) { this.config = config; this.clock = clock; this.document = null; this.jwks = null; this.lastKeys = -Infinity }
  async discover() {
    if (this.document) return this.document
    const raw = `${this.config.issuer.replace(/\/$/u, '')}/.well-known/openid-configuration`
    const document = await boundedJson(raw, this.httpOptions())
    if (document.issuer !== this.config.issuer || !document.response_types_supported?.includes('code') || !document.code_challenge_methods_supported?.includes('S256')) fail('team.invalid_discovery', 502)
    for (const field of ['authorization_endpoint', 'token_endpoint', 'jwks_uri']) {
      const url = new URL(document[field])
      if (!this.config.oidcOrigins.has(url.origin) || url.username || url.password || url.hash || url.search || (url.protocol !== 'https:' && !(this.config.fixture && url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port))) fail('team.untrusted_oidc_endpoint', 502)
    }
    if (document.token_endpoint_auth_methods_supported && !document.token_endpoint_auth_methods_supported.includes(this.config.token_auth_method)) fail('team.unsupported_client_auth', 502)
    this.document = document
    return document
  }
  httpOptions(extra = {}) { return { origins: this.config.oidcOrigins, fixture: this.config.fixture, ...extra } }
  async keys(force = false) {
    const now = this.clock()
    if (this.jwks && !force && now - this.lastKeys < 300000) return this.jwks
    if (force && now - this.lastKeys < 1000) fail('team.key_refresh_limited', 401)
    const doc = await this.discover()
    const keys = await boundedJson(doc.jwks_uri, this.httpOptions())
    if (!Array.isArray(keys.keys) || keys.keys.length < 1 || keys.keys.length > 64) fail('team.invalid_jwks', 502)
    for (const key of keys.keys) {
      if (['d', 'p', 'q', 'dp', 'dq', 'qi', 'k'].some(field => Object.hasOwn(key, field))) fail('team.private_jwks_key', 502)
    }
    this.jwks = createLocalJWKSet(keys); this.lastKeys = now
    return this.jwks
  }
  async verify(token, nonce) {
    text(token, 16384)
    let header
    try { header = decodeProtectedHeader(token) } catch { fail('team.invalid_id_token', 401) }
    if (!['RS256', 'EdDSA'].includes(header.alg) || ['jku', 'jwk', 'x5u', 'x5c'].some(key => Object.hasOwn(header, key)) || typeof header.kid !== 'string') fail('team.invalid_id_token', 401)
    let verified
    const options = { issuer: this.config.issuer, audience: this.config.audience, algorithms: ['RS256', 'EdDSA'], requiredClaims: ['iss', 'sub', 'aud', 'exp', 'iat', 'nonce'], currentDate: new Date(this.clock()), clockTolerance: 30, maxTokenAge: 600 }
    try {
      try { verified = await jwtVerify(token, await this.keys(), options) }
      catch (error) { if (error.code !== 'ERR_JWKS_NO_MATCHING_KEY') throw error; verified = await jwtVerify(token, await this.keys(true), options) }
    } catch { fail('team.invalid_id_token', 401) }
    const payload = verified.payload
    if (!same(payload.nonce, nonce) || (payload.azp !== undefined && payload.azp !== this.config.client_id) || (Array.isArray(payload.aud) && (payload.aud.length !== 1 || payload.aud[0] !== this.config.client_id))) fail('team.invalid_id_token', 401)
    text(payload.sub, 255)
    integer(payload.exp, 1, 253402300799); integer(payload.iat, 1, 253402300799)
    const groups = payload[this.config.groups_claim] ?? []
    if (!Array.isArray(groups) || groups.length > 100 || groups.some(group => typeof group !== 'string' || group.length > 255 || /[\x00-\x1f\x7f]/u.test(group))) fail('team.invalid_groups', 401)
    return { issuer: payload.iss, subject: payload.sub, groups, oidc_expires_at_ms: payload.exp * 1000 }
  }
  async redeem(code, flow) {
    text(code, 8192)
    const doc = await this.discover()
    const form = new URLSearchParams({ grant_type: 'authorization_code', code, redirect_uri: this.config.origin + '/auth/callback', code_verifier: flow.verifier })
    const headers = { 'content-type': 'application/x-www-form-urlencoded' }
    if (this.config.token_auth_method === 'client_secret_basic') headers.authorization = 'Basic ' + Buffer.from(`${encodeURIComponent(this.config.client_id)}:${encodeURIComponent(this.config.client_secret)}`).toString('base64')
    else { form.set('client_id', this.config.client_id); form.set('client_secret', this.config.client_secret) }
    const response = await boundedJson(doc.token_endpoint, this.httpOptions({ method: 'POST', headers, body: form.toString() }))
    if (typeof response.id_token !== 'string' || response.error) fail('team.oidc_denied', 401)
    return this.verify(response.id_token, flow.nonce)
  }
  async authorization(flow) {
    const doc = await this.discover(); const url = new URL(doc.authorization_endpoint)
    for (const [key, value] of Object.entries({ response_type: 'code', client_id: this.config.client_id, redirect_uri: this.config.origin + '/auth/callback', scope: 'openid', state: flow.state, nonce: flow.nonce, code_challenge: flow.challenge, code_challenge_method: 'S256' })) url.searchParams.set(key, value)
    return url.href
  }
}
