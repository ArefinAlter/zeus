// Bounded public contracts. Unknown keys are rejected before persistence or signing.
import { createHash, randomBytes, timingSafeEqual } from 'node:crypto'

export class Failure extends Error {
  constructor(code, status = 400) { super(code); this.code = code; this.status = status }
}
export const fail = (code, status) => { throw new Failure(code, status) }
export const sha = value => createHash('sha256').update(value).digest('hex')
export const random = (bytes = 32) => randomBytes(bytes).toString('base64url')
export function same(a, b) {
  const left = Buffer.from(String(a)); const right = Buffer.from(String(b))
  return left.length === right.length && timingSafeEqual(left, right)
}
export function integer(value, low = 0, high = Number.MAX_SAFE_INTEGER) {
  if (!Number.isSafeInteger(value) || value < low || value > high) fail('team.invalid_number')
  return value
}
export function object(value, allowed, required = allowed) {
  if (!value || typeof value !== 'object' || Array.isArray(value)) fail('team.invalid_object')
  if (Object.keys(value).some(key => !allowed.includes(key)) || required.some(key => !Object.hasOwn(value, key))) fail('team.unknown_or_missing_field')
  return value
}
export function text(value, max = 128, pattern) {
  if (typeof value !== 'string' || value.length < 1 || value.length > max || /[\x00-\x1f\x7f]/u.test(value) || (pattern && !pattern.test(value))) fail('team.invalid_text')
  return value
}
export function id(value) {
  text(value, 64, /^[A-Za-z0-9][A-Za-z0-9_-]*$/u)
  if (/^(?:sk-|sk_ant_|AIza|AKIA|Bearer|ZEUS-CANARY-)/iu.test(value)) fail('team.sensitive_metadata')
  return value
}
export function label(value) {
  text(value, 128, /^[\p{L}\p{N}][\p{L}\p{N} ._()\-]*$/u)
  if (/\b(?:sk-|AIza|AKIA|Bearer|password|secret|token)[A-Za-z0-9_=-]*/iu.test(value)) fail('team.sensitive_metadata')
  return value
}
export function canonical(value) {
  if (value === null || typeof value === 'boolean') return JSON.stringify(value)
  if (typeof value === 'number') return String(integer(value, -Number.MAX_SAFE_INTEGER))
  if (typeof value === 'string') return JSON.stringify(value)
  if (Array.isArray(value)) return `[${value.map(canonical).join(',')}]`
  if (value && typeof value === 'object') return `{${Object.keys(value).sort().map(key => `${JSON.stringify(key)}:${canonical(value[key])}`).join(',')}}`
  fail('team.invalid_json')
}
export function jsonBytes(value, max = 16384) {
  const bytes = Buffer.from(canonical(value))
  if (bytes.length > max) fail('team.payload_too_large', 413)
  return bytes
}
export const FEATURES = Object.freeze(['downloads', 'cloud_delivery', 'workflows', 'metadata_sync', 'provenance'])
export function features(value) {
  object(value, FEATURES)
  for (const key of FEATURES) if (typeof value[key] !== 'boolean') fail('team.invalid_feature_policy')
  return value
}
export function metadata(kind, value) {
  if (kind === 'tags') {
    object(value, ['label', 'color'])
    label(value.label); text(value.color, 7, /^#[0-9a-f]{6}$/u)
  } else if (kind === 'queue_presets') {
    object(value, ['label', 'parallel', 'bytes_per_second', 'schedule_utc'])
    label(value.label); integer(value.parallel, 1, 64); integer(value.bytes_per_second, 0, 1024 ** 4)
    object(value.schedule_utc, ['start_minute', 'end_minute'])
    integer(value.schedule_utc.start_minute, 0, 1439); integer(value.schedule_utc.end_minute, 0, 1439)
  } else if (kind === 'workspace_labels') {
    object(value, ['label', 'tags']); label(value.label)
    if (!Array.isArray(value.tags) || value.tags.length > 32 || new Set(value.tags).size !== value.tags.length) fail('team.invalid_tags')
    value.tags.forEach(id)
  } else fail('team.unsupported_metadata_kind')
  jsonBytes(value)
  return value
}
export function provenance(value) {
  object(value, ['sha256', 'size_bytes', 'source_kind', 'label'], ['sha256', 'size_bytes', 'source_kind'])
  text(value.sha256, 64, /^[0-9a-f]{64}$/u); integer(value.size_bytes)
  if (!['http', 'cloud', 'torrent', 'artifact', 'unknown'].includes(value.source_kind)) fail('team.invalid_source_kind')
  if (value.label !== undefined) label(value.label)
  return value
}
export function publicUrl(value, fixture = false) {
  text(value, 2048)
  let url; try { url = new URL(value) } catch { fail('team.invalid_url') }
  const loopback = fixture && url.protocol === 'http:' && url.hostname === '127.0.0.1' && url.port
  if ((!loopback && url.protocol !== 'https:') || url.username || url.password || url.search || url.hash) fail('team.invalid_url')
  return url
}
export const actorId = identity => sha(`${identity.issuer}\0${identity.subject}`)
export function roleAtLeast(role, wanted) { return ({ reader: 1, writer: 2, admin: 3 }[role] ?? 0) >= ({ reader: 1, writer: 2, admin: 3 }[wanted] ?? 99) }
export function utc(ms) { return new Date(integer(ms)).toISOString() }
export function escapeHtml(value) { return String(value).replace(/[&<>"']/gu, c => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c])) }
