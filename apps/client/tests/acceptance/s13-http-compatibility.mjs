// Native desktop intake -> real daemon -> file on disk, in a fresh isolated profile.
// ZEUS_CLIENT_TARGET and ZEUS_BIN_DIR must point to the freshly built repair binaries.
import assert from 'node:assert/strict'
import { createServer } from 'node:http'
import { existsSync, readFileSync } from 'node:fs'
import { join } from 'node:path'
import { tmpdir } from 'node:os'
import { download, startHarness, until } from '../harness.mjs'

const pdf = Buffer.from('%PDF-1.7\n1 0 obj\n<< /Type /Catalog >>\nendobj\n%%EOF\n')
const requests = []
const server = createServer((req, res) => {
  const path = new URL(req.url, 'http://fixture').pathname
  requests.push({ path, range: req.headers.range ?? null })
  if (path === '/redirect') { res.writeHead(302, { location: '/Quarterly%20Report.pdf' }); res.end(); return }
  if (path === '/forbidden') { res.writeHead(403, { 'content-length': 0 }); res.end(); return }
  if (['/reject416', '/reject400'].includes(path) && req.headers.range) {
    res.writeHead(path === '/reject416' ? 416 : 400, { 'content-length': 0 }); res.end(); return
  }
  const headers = { 'content-type': 'application/pdf', etag: '"fixture-v1"' }
  if (path === '/named') headers['content-disposition'] = "attachment; filename*=UTF-8''Server%20Report.pdf"
  if (path !== '/stream') headers['content-length'] = pdf.length
  res.writeHead(200, headers) // Includes both ignored ranges and unknown-length streaming.
  res.end(pdf)
})
await new Promise(resolve => server.listen(0, '127.0.0.1', resolve))
const origin = `http://127.0.0.1:${server.address().port}`
const home = process.env.ZEUS_HTTP_EVIDENCE ?? join(tmpdir(), `zeus-http-${Date.now()}`)
if (existsSync(home)) throw new Error('Use a new scratch profile')
let h
const checks = []
try {
  h = await startHarness({ home })
  await h.ready()
  for (const [path, expected, filename] of [
    ['/download?id=42', 'download.pdf'],
    ['/get.php?id=42', 'get.pdf'],
    ['/redirect', 'Quarterly Report.pdf'],
    ['/named', 'Server Report.pdf'],
    ['/reject416', 'reject416.pdf'],
    ['/reject400', 'reject400.pdf'],
    ['/stream', 'stream.pdf'],
    ['/custom', 'my-choice.raw', 'my-choice.raw'],
    ['/download?id=43', 'download (1).pdf'],
  ]) {
    const started = await download(h, [{ url: origin + path, ...(filename ? { filename } : {}) }])
    const id = started.jobs[0]?.job_id
    assert.ok(id, `enqueue ${path}`)
    const job = await until(async () => {
      const j = (await h.get('/api/v1/jobs')).items.find(j => j.id === id)
      return j && ['completed', 'failed', 'canceled'].includes(j.state) ? j : false
    }, { timeoutMs: 30000, everyMs: 200, what: `${path} completion` })
    assert.equal(job.state, 'completed', `${path}: ${job.error_code ?? job.last_error?.code}`)
    assert.equal(job.filename, expected)
    assert.deepEqual(readFileSync(job.path), pdf)
    checks.push({ path, filename: job.filename, state: job.state, bytes: job.bytes_done })
  }
  for (const path of ['/reject400', '/reject416']) {
    assert.ok(requests.some(r => r.path === path && r.range === null), `plain GET fallback ${path}`)
  }
  const started = await download(h, [{ url: `${origin}/forbidden` }])
  const id = started.jobs[0]?.job_id
  assert.ok(id)
  const failed = await until(async () => {
    const j = (await h.get('/api/v1/jobs')).items.find(j => j.id === id)
    return j?.state === 'failed' ? j : false
  }, { timeoutMs: 15000, everyMs: 200, what: 'visible forbidden failure' })
  assert.match(failed.error_code ?? failed.last_error?.code ?? '', /403|forbidden/)
  checks.push({ path: '/forbidden', state: failed.state, code: failed.error_code ?? failed.last_error?.code })
  console.log(JSON.stringify({ passed: true, checks, backend: 'desktop route table + real daemon + disk verification' }, null, 2))
} finally {
  await h?.stop()
  await new Promise(resolve => server.close(resolve))
}
