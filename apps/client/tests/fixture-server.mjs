// A loopback HTTP fixture for the acceptance runs and the Rust integration tests: files of any size with real range
// support, and the failure modes a download manager must handle. No dependencies.
//
//   node tests/fixture-server.mjs [port]        prints {"url":"http://127.0.0.1:PORT"} and serves until stopped
//
// Routes (every file body is deterministic: byte i depends only on the seed and i, so a resumed download can be checked):
//   /files/<name>?size=N&seed=S&rate=KiBps   ranged 200/206, strong ETag, Last-Modified, optional throttle
//   /norange/<name>?size=N                    ignores Range, always 200 (Accept-Ranges: none)
//   /redirect/<n>/<name>?size=N               n hops of 302, then /files/<name>
//   /cd/<name>?size=N&as=<filename>           Content-Disposition: attachment; filename="..." (and filename*=)
//   /expired/<name>                           403 (a signed link that expired)
//   /gone/<name>                              410
//   /missing/<name>                           404
//   /busy/<name>?size=N&fails=K&after=S       503 with Retry-After: S for the first K requests, then the file
//   /dropped/<name>?size=N&cut=F             no ranges or validators; the first request loses its connection after fraction F (a browser download that cannot resume)
//   /flaky/<name>?size=N&cut=F                closes the connection after fraction F of the body, the first time only
//   /hash/<name>?size=N&seed=S                the SHA-256 of that file, as text
//   /log                                      JSON list of the requests seen (method, path, range)
//   /site/...                                 a small crawlable site (see below)

import { createHash } from 'node:crypto'
import { createServer } from 'node:http'

const MASK = (1n << 64n) - 1n

function mix(x) {
  x = (x + 0x9e3779b97f4a7c15n) & MASK
  x = ((x ^ (x >> 30n)) * 0xbf58476d1ce4e5b9n) & MASK
  x = ((x ^ (x >> 27n)) * 0x94d049bb133111ebn) & MASK
  return (x ^ (x >> 31n)) & MASK
}

/** Bytes [start, end) of the deterministic stream for a seed. */
export function bytes(seed, start, end) {
  const out = Buffer.allocUnsafe(end - start)
  let pos = start
  while (pos < end) {
    const block = BigInt(Math.floor(pos / 8))
    const word = mix(block ^ (BigInt(seed) << 40n))
    const offset = pos % 8
    const take = Math.min(8 - offset, end - pos)
    for (let i = 0; i < take; i++) out[pos - start + i] = Number((word >> BigInt((offset + i) * 8)) & 0xffn)
    pos += take
  }
  return out
}

export function sha256(seed, size) {
  const h = createHash('sha256')
  const step = 1 << 20
  for (let p = 0; p < size; p += step) h.update(bytes(seed, p, Math.min(size, p + step)))
  return h.digest('hex')
}

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

function parseRange(header, size) {
  const m = /^bytes=(\d*)-(\d*)$/.exec(header ?? '')
  if (!m) return null
  let start = m[1] === '' ? null : Number(m[1])
  let end = m[2] === '' ? null : Number(m[2])
  if (start === null && end === null) return null
  if (start === null) {
    start = Math.max(0, size - end)
    end = size - 1
  } else if (end === null || end >= size) end = size - 1
  if (start > end || start >= size) return 'unsatisfiable'
  return { start, end }
}

export function startFixtures(port = 0) {
  const seen = []
  const busy = new Map()
  const flaked = new Set()
  const droppedSeen = new Set()
  const siteHits = []
  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? '/', 'http://x')
    const parts = url.pathname.split('/').filter(Boolean)
    seen.push({ method: req.method, path: url.pathname + url.search, range: req.headers.range ?? null, ua: req.headers['user-agent'] ?? null, t: Date.now() })
    if (seen.length > 5000) seen.shift()
    const q = (k, d) => url.searchParams.get(k) ?? d
    const size = Number(q('size', 1 << 20))
    const seed = Number(q('seed', 1))
    const kind = parts[0]

    if (url.pathname === '/robots.txt') {
      // The crawlable site's robots.txt: one private area, and a crawl delay of one second.
      res.writeHead(200, { 'content-type': 'text/plain' })
      return res.end(['# fixture', 'User-agent: *', 'Disallow: /site/private/', 'Crawl-delay: 1', ''].join('\n'))
    }
    if (kind === 'log') {
      res.writeHead(200, { 'content-type': 'application/json' })
      return res.end(JSON.stringify(seen))
    }
    if (kind === 'dropped') {
      // A browser's download that cannot be resumed: no validators, no ranges, and the first request loses its
      // connection after a fraction of the body. Later requests (Zeus's own) get the whole file.
      const key = url.pathname + url.search
      const first = !droppedSeen.has(key)
      droppedSeen.add(key)
      res.writeHead(200, { 'content-type': 'application/octet-stream', 'content-length': String(size), 'content-disposition': `attachment; filename="${decodeURIComponent(parts[parts.length - 1] ?? 'file.bin')}"` })
      if (req.method === 'HEAD') return res.end()
      const stop = first ? Math.floor(size * Number(q('cut', 0.4))) : size
      let aborted = false
      req.on('close', () => (aborted = true))
      for (let p = 0; p < stop && !aborted; p += 256 * 1024) {
        if (!res.write(bytes(seed, p, Math.min(stop, p + 256 * 1024)))) await new Promise((r) => res.once('drain', r))
        await sleep(20)
      }
      return first ? res.destroy() : res.end()
    }
    if (kind === 'hash') {
      res.writeHead(200, { 'content-type': 'text/plain' })
      return res.end(sha256(seed, size))
    }
    if (kind === 'site') return site(req, res, url, siteHits)
    if (kind === 'expired') {
      res.writeHead(403, { 'content-type': 'text/plain' })
      return res.end('Request has expired')
    }
    if (kind === 'gone') {
      res.writeHead(410, { 'content-type': 'text/plain' })
      return res.end('Gone')
    }
    if (kind === 'missing') {
      res.writeHead(404, { 'content-type': 'text/plain' })
      return res.end('Not found')
    }
    if (kind === 'redirect') {
      const hops = Number(parts[1] ?? 1)
      const rest = parts.slice(2).join('/')
      const next = hops > 1 ? `/redirect/${hops - 1}/${rest}${url.search}` : `/files/${rest}${url.search}`
      res.writeHead(302, { location: next })
      return res.end()
    }
    if (kind === 'busy') {
      const key = url.pathname + url.search
      const n = (busy.get(key) ?? 0) + 1
      busy.set(key, n)
      if (n <= Number(q('fails', 2))) {
        res.writeHead(503, { 'retry-after': q('after', '2'), 'content-type': 'text/plain' })
        return res.end('Busy')
      }
    }
    if (!['files', 'norange', 'cd', 'busy', 'flaky', 'exe'].includes(kind ?? '')) {
      res.writeHead(404, { 'content-type': 'text/plain' })
      return res.end('unknown fixture route')
    }

    const name = decodeURIComponent(parts[parts.length - 1] ?? 'file.bin')
    const etag = `"s${seed}-n${size}"`
    const headers = {
      etag,
      'last-modified': 'Mon, 05 Oct 2026 08:00:00 GMT',
      'content-type': name.endsWith('.txt') ? 'text/plain' : name.endsWith('.png') ? 'image/png' : 'application/octet-stream',
    }
    if (kind === 'cd') {
      const as = q('as', 'report final.pdf')
      headers['content-disposition'] = `attachment; filename="${as.replace(/"/g, '')}"; filename*=UTF-8''${encodeURIComponent(as)}`
    }
    const ranged = kind !== 'norange'
    headers['accept-ranges'] = ranged ? 'bytes' : 'none'
    const range = ranged ? parseRange(req.headers.range, size) : null
    if (req.headers['if-range'] && req.headers['if-range'] !== etag) {
      // The representation changed: send the whole thing.
    }
    if (range === 'unsatisfiable') {
      res.writeHead(416, { 'content-range': `bytes */${size}` })
      return res.end()
    }
    const start = range ? range.start : 0
    const end = range ? range.end + 1 : size
    headers['content-length'] = String(end - start)
    if (range) headers['content-range'] = `bytes ${range.start}-${range.end}/${size}`
    res.writeHead(range ? 206 : 200, headers)
    if (req.method === 'HEAD') return res.end()

    const rate = Number(q('rate', 0)) * 1024
    const cutAt = kind === 'flaky' && !flaked.has(url.pathname + url.search) ? Math.floor((end - start) * Number(q('cut', 0.5))) : null
    if (cutAt !== null) flaked.add(url.pathname + url.search)
    const chunk = rate ? Math.max(4096, Math.floor(rate / 20)) : 256 * 1024
    let sent = 0
    let aborted = false
    req.on('close', () => (aborted = true))
    for (let p = start; p < end && !aborted; p += chunk) {
      const to = Math.min(end, p + chunk)
      if (cutAt !== null && sent + (to - p) > cutAt) {
        res.write(bytes(seed, p, p + (cutAt - sent)))
        return res.destroy()
      }
      if (!res.write(bytes(seed, p, to))) await new Promise((r) => res.once('drain', r))
      sent += to - p
      if (rate) await sleep(((to - p) / rate) * 1000)
    }
    res.end()
  })
  return new Promise((resolve) => {
    server.listen(port, '127.0.0.1', () => {
      const { port: p } = server.address()
      resolve({
        url: `http://127.0.0.1:${p}`,
        port: p,
        log: seen,
        siteHits,
        close: () => new Promise((r) => (server.closeAllConnections?.(), server.close(() => r()))),
      })
    })
  })
}

// ---------------------------------------------------------------- the crawlable site (scenario: site grabber)

const PAGES = {
  '/site/': { title: 'Handbook', links: ['/site/guide/', '/site/files/report.pdf', '/site/files/data.zip', '/site/blog/', 'https://elsewhere.example.org/x.zip', '/site/private/secret.pdf', '/site/loop/', '/site/missing/', '/site/get', '/site/files/report.pdf#top'] },
  '/site/loop/': { title: 'Loop', links: ['/site/loop/', '/site/', '/site/loop/?again=1'] },
  '/site/guide/': { title: 'Guide', links: ['/site/', '/site/guide/chapter1/', '/site/files/guide.pdf', '/site/files/notes.txt'] },
  '/site/guide/chapter1/': { title: 'Chapter 1', links: ['/site/guide/', '/site/files/chapter1.pdf', '/site/guide/chapter1/deep/'] },
  '/site/guide/chapter1/deep/': { title: 'Deep', links: ['/site/files/deep.zip'] },
  '/site/blog/': { title: 'Blog', links: ['/site/blog/post1/', '/site/files/blog.zip'] },
  '/site/blog/post1/': { title: 'Post', links: ['/site/files/post.png'] },
}

/** The deterministic size and seed of a file of the crawlable site (so a test can compute its SHA-256). */
export function siteFile(name) {
  return { size: 20_000 + name.length * 1111, seed: [...name].reduce((n, c) => n + c.charCodeAt(0), 0) }
}

function site(req, res, url, hits) {
  hits.push({ method: req.method, path: url.pathname, t: Date.now(), ua: req.headers['user-agent'] ?? '' })
  if (url.pathname === '/site/get') {
    // A link without an extension that is not a page: a PDF.
    res.writeHead(200, { 'content-type': 'application/pdf', 'content-length': '13' })
    return res.end('%PDF-1.4 fake')
  }
  if (url.pathname.startsWith('/site/files/')) {
    const name = decodeURIComponent(url.pathname.split('/').pop() ?? 'f')
    const { size, seed } = siteFile(name)
    const range = parseRange(req.headers.range, size)
    if (range === 'unsatisfiable') {
      res.writeHead(416, { 'content-range': `bytes */${size}` })
      return res.end()
    }
    const start = range ? range.start : 0
    const end = range ? range.end + 1 : size
    const headers = { 'content-type': name.endsWith('.png') ? 'image/png' : 'application/octet-stream', 'content-length': String(end - start), 'accept-ranges': 'bytes', etag: `"site-${name}-${size}"`, 'last-modified': 'Mon, 05 Oct 2026 08:00:00 GMT' }
    if (range) headers['content-range'] = `bytes ${range.start}-${range.end}/${size}`
    res.writeHead(range ? 206 : 200, headers)
    return res.end(req.method === 'HEAD' ? undefined : bytes(seed, start, end))
  }
  const page = PAGES[url.pathname]
  if (!page) {
    res.writeHead(404, { 'content-type': 'text/plain' })
    return res.end('no such page')
  }
  const links = page.links.map((l) => `<a href="${l.startsWith('http') ? l : l}">${l}</a>`).join('\n')
  res.writeHead(200, { 'content-type': 'text/html; charset=utf-8' })
  res.end(`<!doctype html><title>${page.title}</title><h1>${page.title}</h1>\n${links}\n`)
}

if (import.meta.url === `file://${process.argv[1]?.replace(/\\/g, '/')}` || process.argv[1]?.endsWith('fixture-server.mjs')) {
  const f = await startFixtures(Number(process.argv[2] ?? 0))
  console.log(JSON.stringify({ url: f.url }))
}
