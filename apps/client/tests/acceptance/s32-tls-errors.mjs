// Real HTTPS failure through the desktop -> daemon -> production rustls transport.
// Generates a disposable localhost certificate; never changes the machine trust store.
import assert from 'node:assert/strict'
import { execFileSync } from 'node:child_process'
import { createServer } from 'node:https'
import { existsSync, mkdirSync, readFileSync, rmSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { sleep, startHarness, until } from '../harness.mjs'

const home = process.env.ZEUS_TLS_EVIDENCE ?? join(tmpdir(), `zeus-tls-${Date.now()}`)
if (existsSync(home)) throw new Error('Use a new scratch profile')
mkdirSync(home, { recursive: true })
const key = join(home, 'disposable-localhost-key.pem')
const cert = join(home, 'disposable-localhost-cert.pem')
let h, server
let connections = 0, requests = 0
try {
  execFileSync(process.env.ZEUS_TEST_OPENSSL ?? 'openssl', [
    'req', '-x509', '-newkey', 'rsa:2048', '-nodes', '-days', '1',
    '-subj', '/CN=Zeus disposable localhost test', '-addext', 'subjectAltName=IP:127.0.0.1',
    '-keyout', key, '-out', cert,
  ], { stdio: 'ignore', timeout: 30000, windowsHide: true })
  server = createServer({ key: readFileSync(key), cert: readFileSync(cert) }, (_req, res) => {
    requests++
    res.writeHead(200, { 'content-type': 'application/pdf' })
    res.end('%PDF-1.7\n%%EOF\n')
  })
  server.on('connection', () => connections++)
  server.on('tlsClientError', () => {})
  await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
  h = await startHarness({ home })
  await h.ready()
  const review = await h.post('/api/v1/links/preview', { items: [{ url: `https://127.0.0.1:${server.address().port}/untrusted.pdf` }] })
  assert.ok(review.ready)
  assert.equal(connections, 1, 'the read-only preview probes once')
  const started = await h.post('/api/v1/links/enqueue', { review_id: review.review_id, idempotency_key: 'tls-localhost-fixture' })
  const id = started.jobs[0]?.job_id
  assert.ok(id)
  const job = await until(async () => {
    const item = (await h.get('/api/v1/jobs')).items.find(item => item.id === id)
    return item && ['failed', 'retry_scheduled', 'completed'].includes(item.state) ? item : false
  }, { timeoutMs: 15000, everyMs: 100, what: 'certificate failure' })
  const code = job.error_code ?? job.last_error?.code
  console.log(JSON.stringify({ state: job.state, code, connections, http_requests: requests }))
  assert.equal(code, 'tls.cert_invalid')
  assert.equal(job.state, 'failed')
  await sleep(1500)
  assert.equal(connections, 2, 'one preview and one job attempt; invalid certificates must not be retried')
  assert.equal(requests, 0, 'no HTTP request may pass certificate validation')
  console.log(JSON.stringify({ passed: true, certificate_validation_bypassed: false, automatic_retries: 0 }))
} finally {
  await h?.stop()
  if (server?.listening) await new Promise(resolve => server.close(resolve))
  rmSync(key, { force: true })
}
