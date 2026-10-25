// Request hashes and bind codes for approvals, after Zeus spec 07 sections 4.6 and 7.7. A decision binds to the hash
// of the exact facts a person saw; a changed fact is a new revision with a new hash, which clears typed input and
// restarts the arming delay. The bind code is 30 bits of the hash in Crockford base 32, typed for `typed_code`
// confirmations and read case-insensitively with O as 0 and I or L as 1.
//
// zeusd hashes the JCS form of the facts with BLAKE3. This client has no BLAKE3, so it computes SHA-256 over the same
// canonical form as a stand-in; the About page says so. Pure functions, tested with node --test.

import type { ApprovalKind, Scope } from './types.ts'

/** Canonical JSON: object keys sorted by code unit, no whitespace. Integers, strings, booleans, null, arrays, objects. */
export function canonical(v: unknown): string {
  if (v === null || typeof v === 'boolean') return JSON.stringify(v)
  if (typeof v === 'number') {
    if (!Number.isSafeInteger(v)) throw new Error('canonical JSON here carries integers only')
    return String(v)
  }
  if (typeof v === 'string') return JSON.stringify(v)
  if (Array.isArray(v)) return `[${v.map(canonical).join(',')}]`
  if (typeof v === 'object') {
    const o = v as Record<string, unknown>
    const keys = Object.keys(o)
      .filter((k) => o[k] !== undefined)
      .sort()
    return `{${keys.map((k) => `${JSON.stringify(k)}:${canonical(o[k])}`).join(',')}}`
  }
  throw new Error(`cannot put ${typeof v} in canonical JSON`)
}

const K = new Uint32Array([
  0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5, 0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
  0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da, 0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
  0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85, 0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
  0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3, 0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
])

function utf8(s: string): Uint8Array {
  return new TextEncoder().encode(s)
}

/** SHA-256 of a string's UTF-8 bytes, as lower-case hex. */
export function sha256Hex(text: string): string {
  const msg = utf8(text)
  const len = msg.length
  const blocks = Math.ceil((len + 9) / 64)
  const buf = new Uint8Array(blocks * 64)
  buf.set(msg)
  buf[len] = 0x80
  const bits = len * 8
  const view = new DataView(buf.buffer)
  view.setUint32(buf.length - 8, Math.floor(bits / 0x100000000))
  view.setUint32(buf.length - 4, bits >>> 0)
  const h = new Uint32Array([0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19])
  const w = new Uint32Array(64)
  const rotr = (x: number, n: number) => (x >>> n) | (x << (32 - n))
  for (let b = 0; b < blocks; b++) {
    for (let i = 0; i < 16; i++) w[i] = view.getUint32(b * 64 + i * 4)
    for (let i = 16; i < 64; i++) {
      const w15 = w[i - 15] as number
      const w2 = w[i - 2] as number
      const s0 = rotr(w15, 7) ^ rotr(w15, 18) ^ (w15 >>> 3)
      const s1 = rotr(w2, 17) ^ rotr(w2, 19) ^ (w2 >>> 10)
      w[i] = ((w[i - 16] as number) + s0 + (w[i - 7] as number) + s1) >>> 0
    }
    let [a, bb, c, d, e, f, g, hh] = Array.from(h) as [number, number, number, number, number, number, number, number]
    for (let i = 0; i < 64; i++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25)
      const ch = (e & f) ^ (~e & g)
      const t1 = (hh + S1 + ch + (K[i] as number) + (w[i] as number)) >>> 0
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22)
      const maj = (a & bb) ^ (a & c) ^ (bb & c)
      const t2 = (S0 + maj) >>> 0
      hh = g
      g = f
      f = e
      e = (d + t1) >>> 0
      d = c
      c = bb
      bb = a
      a = (t1 + t2) >>> 0
    }
    h[0] = ((h[0] as number) + a) >>> 0
    h[1] = ((h[1] as number) + bb) >>> 0
    h[2] = ((h[2] as number) + c) >>> 0
    h[3] = ((h[3] as number) + d) >>> 0
    h[4] = ((h[4] as number) + e) >>> 0
    h[5] = ((h[5] as number) + f) >>> 0
    h[6] = ((h[6] as number) + g) >>> 0
    h[7] = ((h[7] as number) + hh) >>> 0
  }
  return Array.from(h, (x) => x.toString(16).padStart(8, '0')).join('')
}

const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ'

/** The 30-bit bind code of a hex digest: its first 30 bits in Crockford base 32, as `XXX-XXX`. */
export function bindCode(hex: string): string {
  const n = parseInt(hex.slice(0, 8), 16) >>> 2 // 30 bits
  let s = ''
  for (let i = 5; i >= 0; i--) s += CROCKFORD[(n >>> (i * 5)) & 31]
  return `${s.slice(0, 3)}-${s.slice(3)}`
}

/** A typed code as the comparison sees it: upper case, O as 0, I and L as 1, no spaces or hyphens. */
export function normalizeCode(typed: string): string {
  return typed
    .toUpperCase()
    .replace(/[\s-]/g, '')
    .replace(/O/g, '0')
    .replace(/[IL]/g, '1')
}

export function codeMatches(typed: string, code: string): boolean {
  return normalizeCode(typed) === normalizeCode(code)
}

/** The hash a decision binds to. */
export function requestHash(facts: unknown): string {
  return sha256Hex(canonical(facts))
}

/** Which scopes the daemon offers for a kind (spec 07 section 4.1); computed from the request, never from a wish. */
export function allowedScopes(kind: ApprovalKind, opts: { coversJobs: boolean; namesRun: boolean }): Scope[] {
  switch (kind) {
    case 'handoff':
      return ['once', 'site', 'never']
    case 'session_grant': {
      const out: Scope[] = []
      if (opts.coversJobs) out.push('once')
      out.push('site')
      if (opts.namesRun) out.push('workflow')
      out.push('never')
      return out
    }
    case 'workflow_plan':
      return opts.namesRun ? ['workflow'] : []
    default:
      return []
  }
}

export function confirmationFor(kind: ApprovalKind, scope?: Scope): 'gesture' | 'typed_code' {
  if (kind === 'handoff' || kind === 'retry_escalation') return 'gesture'
  if (kind === 'session_grant') return scope === 'workflow' ? 'typed_code' : 'gesture'
  return 'typed_code'
}

/** "Lasts" as the brand guidelines write scopes: once, 24 hours (site), one plan (workflow), never (block). */
export const SCOPE_LASTS: Record<Scope, string> = { once: 'once', site: '24 hours (site)', workflow: 'one plan (workflow)', never: 'never (block)' }

/** The button for each scope names exactly what happens. */
export const SCOPE_BUTTON: Record<Scope, string> = { once: 'Allow once', site: 'Allow for 24 hours', workflow: 'Allow for this plan', never: 'Block this site' }

export const KIND_LABEL: Record<ApprovalKind, string> = {
  handoff: 'Browser handoff',
  session_grant: 'Site login',
  retry_escalation: 'Retry after repeated failure',
  policy_exception: 'Policy exception',
  workflow_plan: 'Workflow run',
  destructive_op: 'File operation',
  cloud_delivery: 'Cloud delivery',
}

/** The arming delay of an approve button, in milliseconds (spec 07 section 10.1, brand trust-surface rule). */
export const ARMING_MS = 750
