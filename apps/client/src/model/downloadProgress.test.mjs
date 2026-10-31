import assert from 'node:assert/strict'
import { test } from 'node:test'
import { computeTransferProgress } from './segments.ts'
import { errorOf } from './downloadErrors.ts'

test('live progress preserves committed bytes without fabricating parallel workers', () => {
  const segments = computeTransferProgress(1_000_000, 321_777, true)
  assert.equal(segments.length, 1)
  assert.equal(segments.reduce((n, s) => n + s.done, 0), 321_777)
  assert.equal(computeTransferProgress(null, 512, true)[0].done, 512)
  assert.deepEqual(computeTransferProgress(null, 0, true), [])
  assert.equal(computeTransferProgress(100, 200, false)[0].done, 100)
})

test('actual daemon failure codes give actionable messages', () => {
  assert.match(errorOf('http.status_403').message, /refused/)
  assert.match(errorOf('http.blocked_challenge').next, /extension/)
  assert.match(errorOf('url.expired').next, /fresh link/)
  assert.match(errorOf('net.read_timeout').message, /stopped sending/)
  assert.match(errorOf('http.status_429').message, /limited/)
  assert.equal(errorOf(undefined), undefined)
})


test('certificate errors explain the permanent failure instead of suggesting a retry', () => {
  assert.match(errorOf('tls.cert_invalid').message, /certificate/)
  assert.match(errorOf('tls.cert_invalid').next, /not retried/)
  assert.match(errorOf('tls.handshake_failed').message, /secure connection/)
})
