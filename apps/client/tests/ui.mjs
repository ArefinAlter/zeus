// Drives the real interface in Chromium against the real engine (the loopback harness), for the acceptance runs.
//
//   const ui = await openUi(h)            // h from startHarness(); serves dist/harness (npm run build:harness)
//   await ui.page.click('text=Add URL')
//   await ui.close()
//
// Playwright comes from the Dynodoc checkout on this machine (no copy in this repository).

import { createReadStream, existsSync, statSync } from 'node:fs'
import { createServer } from 'node:http'
import { createRequire } from 'node:module'
import { extname, join, normalize, resolve, sep } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = fileURLToPath(new URL('.', import.meta.url))
const require = createRequire('B:/dynodoc/frontend/researcher/package.json')
const { chromium } = require('@playwright/test')

const TYPES = { '.html': 'text/html; charset=utf-8', '.js': 'text/javascript', '.css': 'text/css', '.svg': 'image/svg+xml', '.png': 'image/png', '.json': 'application/json', '.woff2': 'font/woff2' }

/** Serve a built folder on a free loopback port. */
export function serveDir(dir) {
  dir = resolve(dir)
  const server = createServer((req, res) => {
    const url = new URL(req.url ?? '/', 'http://x')
    let p = normalize(join(dir, decodeURIComponent(url.pathname)))
    if (p !== dir && !p.startsWith(dir + sep)) return res.writeHead(403).end()
    if (existsSync(p) && statSync(p).isDirectory()) p = join(p, 'index.html')
    if (!existsSync(p)) return res.writeHead(404).end('not found')
    res.writeHead(200, { 'content-type': TYPES[extname(p)] ?? 'application/octet-stream' })
    createReadStream(p).pipe(res)
  })
  return new Promise((resolve) => server.listen(0, '127.0.0.1', () => resolve({ port: server.address().port, close: () => new Promise((r) => server.close(r)) })))
}

export async function openUi(h, { width = 1360, height = 880, theme } = {}) {
  // ZEUS_UI_DIR lets parallel runs serve their own build (vite build --mode harness --outDir dist/<name>).
  const dir = process.env.ZEUS_UI_DIR ?? join(here, '..', 'dist', 'harness')
  if (!existsSync(join(dir, 'index.html'))) throw new Error('build the interface first: npm run build:harness')
  const site = await serveDir(dir)
  const browser = await chromium.launch({ headless: true })
  const context = await browser.newContext({ viewport: { width, height }, ...(theme ? { colorScheme: theme } : {}) })
  const page = await context.newPage()
  page.setDefaultTimeout(15000)
  page.setDefaultNavigationTimeout(30000)
  const errors = []
  page.on('pageerror', (e) => errors.push(String(e)))
  page.on('console', (m) => m.type() === 'error' && errors.push(`${m.text()} ${m.location()?.url ?? ''}`))
  try {
    await page.goto(`http://127.0.0.1:${site.port}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}`)
    await page.waitForSelector('.statusbar', { timeout: 30000 })
  } catch (error) {
    await browser.close()
    await site.close()
    throw error
  }
  return {
    page,
    context,
    browser,
    errors,
    origin: `http://127.0.0.1:${site.port}`,
    async close() {
      await browser.close()
      await site.close()
    },
  }
}

/** The origins the harness must allow for a UI served on `port` (call before startHarness via env). */
export const ORIGINS = (ports) => ports.flatMap((p) => [`http://127.0.0.1:${p}`, `http://localhost:${p}`]).join(',')
