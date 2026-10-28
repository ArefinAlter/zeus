// Distinct DSSE domains keep team restrictions and client-reported provenance separate.
import { createPrivateKey, createPublicKey, sign, verify } from 'node:crypto'
import { actorId, canonical, fail, features, id, integer, jsonBytes, object, provenance, text, utc } from './contracts.mjs'

export const POLICY_TYPE = 'application/vnd.verne.zeus.team-policy+json;version=1'
export const PROVENANCE_TYPE = 'application/vnd.verne.zeus.team-provenance+json;version=1'
export const POLICY_VERSION = 1
export function pae(type, payload) {
  return Buffer.concat([Buffer.from(`DSSEv1 ${Buffer.byteLength(type)} ${type} ${payload.length} `), payload])
}
export class Publisher {
  constructor({ issuer, activeKey, keys }) {
    this.issuer = issuer; this.activeKey = id(activeKey)
    this.keys = new Map(Object.entries(keys).map(([kid, pem]) => [id(kid), createPrivateKey(pem)]))
    if (!this.keys.has(this.activeKey) || [...this.keys.values()].some(key => key.asymmetricKeyType !== 'ed25519')) fail('team.invalid_signing_key', 500)
  }
  publicKeys() {
    return [...this.keys.entries()].map(([key_id, privateKey]) => ({ key_id, spki: createPublicKey(privateKey).export({ type: 'spki', format: 'pem' }).toString() }))
  }
  envelope(type, value) {
    const payload = jsonBytes(value, 65536)
    return { payloadType: type, payload: payload.toString('base64'), signatures: [{ keyid: this.activeKey, sig: sign(null, pae(type, payload), this.keys.get(this.activeKey)).toString('base64') }] }
  }
  policy(team, sequence, enabled, now, lifetime = 900000) {
    id(team); integer(sequence, 1); features(enabled); integer(lifetime, 60000, 86400000)
    return this.envelope(POLICY_TYPE, { schema_version: 1, issuer: this.issuer, tenant_id: team, sequence, issued_at: utc(now), expires_at: utc(now + lifetime), min_policy_level: 1, features: enabled })
  }
  provenance(team, actor, value, now) {
    provenance(value)
    return this.envelope(PROVENANCE_TYPE, { schema_version: 1, issuer: this.issuer, tenant_id: id(team), assertion_kind: 'client_reported', actor_id: actorId(actor), observed_at: utc(now), artifact: value })
  }
}
export function verifyEnvelope(envelope, type, pins) {
  object(envelope, ['payloadType', 'payload', 'signatures'])
  if (envelope.payloadType !== type || !Array.isArray(envelope.signatures) || envelope.signatures.length < 1 || envelope.signatures.length > 8) fail('team.invalid_signature')
  text(envelope.payload, 90000)
  const payload = Buffer.from(envelope.payload, 'base64')
  if (payload.length > 65536 || payload.toString('base64') !== envelope.payload) fail('team.invalid_signature')
  const valid = envelope.signatures.some(signature => {
    try {
      object(signature, ['keyid', 'sig']); id(signature.keyid); text(signature.sig, 128)
      if (!Object.hasOwn(pins, signature.keyid)) return false
      const key = createPublicKey(pins[signature.keyid]); const bytes = Buffer.from(signature.sig, 'base64')
      return key.asymmetricKeyType === 'ed25519' && bytes.length === 64 && bytes.toString('base64') === signature.sig && verify(null, pae(type, payload), key, bytes)
    } catch { return false }
  })
  if (!valid) fail('team.untrusted_signature')
  let value; try { value = JSON.parse(payload) } catch { fail('team.invalid_signed_payload') }
  if (Buffer.from(canonical(value)).compare(payload) !== 0) fail('team.noncanonical_signed_payload')
  return value
}
