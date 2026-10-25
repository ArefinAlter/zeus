// Copies the website build into a zeus-site checkout, where Vercel serves it at /client/:
//
//   npm run build:site && node scripts/publish-site.mjs ../../../zeus-site
//
// The site is static with no install step, so the build goes in as files under public/client/. The copy is marked
// noindex (a link to share, not a page to find by search) and keeps no browser storage, because the site promises
// none (VITE_ZEUS_NO_STORAGE). It loads only its own files, so it works under the site's Content Security Policy.
// Nothing is committed or pushed here.

import { cpSync, existsSync, mkdirSync, readFileSync, readdirSync, rmSync, writeFileSync } from 'node:fs'
import { dirname, join, resolve } from 'node:path'
import { fileURLToPath } from 'node:url'

const here = dirname(fileURLToPath(import.meta.url))
const src = resolve(here, '../dist/site')
const site = resolve(process.argv[2] ?? '')
if (!process.argv[2] || !existsSync(join(site, 'public')) || !existsSync(join(site, 'build.mjs'))) {
  console.error('usage: node scripts/publish-site.mjs <path to the zeus-site checkout>')
  process.exit(2)
}
if (!existsSync(join(src, 'index.html'))) {
  console.error('build it first: npm run build:site')
  process.exit(2)
}

const dest = join(site, 'public', 'client')
rmSync(dest, { recursive: true, force: true })
mkdirSync(dest, { recursive: true })
cpSync(src, dest, { recursive: true })

const page = join(dest, 'index.html')
let html = readFileSync(page, 'utf8')
if (!html.includes('name="robots"')) html = html.replace('<meta name="color-scheme"', '<meta name="robots" content="noindex, nofollow" />\n    <meta name="color-scheme"')
if (/<script(?![^>]*\bsrc=)[^>]*>/i.test(html) || /\sstyle=/i.test(html)) {
  console.error('index.html has an inline script or style attribute, which the site CSP refuses')
  process.exit(1)
}
writeFileSync(page, html)

const files = (d) => readdirSync(d, { withFileTypes: true }).flatMap((e) => (e.isDirectory() ? files(join(d, e.name)) : [join(d, e.name)]))
console.log(`copied ${files(dest).length} files to ${dest}`)
