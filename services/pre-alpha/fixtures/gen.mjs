// Generates the fixture sites the hosted pre-alpha may download from, and the catalogue the bridge and the service
// read (hosts, file sizes, suggested links). Run once in a node:24-alpine container with the fixture volume at /srv:
//
//   docker run --rm -v zeus-prealpha_fixtures:/srv -v ./fixtures:/gen:ro node:24-alpine node /gen/gen.mjs
//
// Every file is sample content: text files are readable, binaries are random bytes with the extension of their type,
// the Linux tarball is a real gzip tar, and checksums.txt holds real SHA-256 sums. Nothing here comes from the internet.

import { execFileSync } from 'node:child_process'
import { createHash, randomFillSync } from 'node:crypto'
import { closeSync, existsSync, mkdirSync, openSync, readdirSync, readFileSync, statSync, writeFileSync, writeSync } from 'node:fs'
import { dirname, join, relative } from 'node:path'

const SRV = process.env.SRV ?? '/srv'
const KiB = 1024
const MiB = 1024 * KiB
const FILES = 'files.example.org'
const DOCS = 'docs.example.org'
const MIRROR = 'mirror.example.net'

function ensure(p) {
  mkdirSync(dirname(p), { recursive: true })
}

function random(path, size) {
  size = Math.round(size)
  ensure(path)
  if (existsSync(path) && statSync(path).size === size) return
  const fd = openSync(path, 'w')
  const chunk = Buffer.alloc(Math.min(size, 4 * MiB))
  let left = size
  while (left > 0) {
    const n = Math.min(left, chunk.length)
    randomFillSync(chunk, 0, n)
    writeSync(fd, chunk, 0, n)
    left -= n
  }
  closeSync(fd)
}

function text(path, content) {
  ensure(path)
  writeFileSync(path, content)
}

function lines(n, f) {
  return Array.from({ length: n }, (_, i) => f(i)).join('\n') + '\n'
}

function pdf(title, pages) {
  // A minimal, valid single-font PDF with one line of text per page.
  const objs = []
  const kids = []
  objs.push('<< /Type /Catalog /Pages 2 0 R >>')
  objs.push(null)
  objs.push('<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>')
  for (let i = 0; i < pages; i++) {
    const body = `BT /F1 18 Tf 72 720 Td (${title} - page ${i + 1} of ${pages}) Tj ET\nBT /F1 11 Tf 72 690 Td (Sample document for the Zeus pre-alpha. Generated text, no real content.) Tj ET`
    objs.push(`<< /Length ${body.length} >>\nstream\n${body}\nendstream`)
    const content = objs.length
    objs.push(`<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 3 0 R >> >> /Contents ${content} 0 R >>`)
    kids.push(`${objs.length} 0 R`)
  }
  objs[1] = `<< /Type /Pages /Kids [${kids.join(' ')}] /Count ${pages} >>`
  let out = '%PDF-1.4\n'
  const offsets = []
  objs.forEach((o, i) => {
    offsets.push(Buffer.byteLength(out))
    out += `${i + 1} 0 obj\n${o}\nendobj\n`
  })
  const xref = Buffer.byteLength(out)
  out += `xref\n0 ${objs.length + 1}\n0000000000 65535 f \n${offsets.map((o) => `${String(o).padStart(10, '0')} 00000 n \n`).join('')}`
  out += `trailer\n<< /Size ${objs.length + 1} /Root 1 0 R >>\nstartxref\n${xref}\n%%EOF\n`
  return out
}

function padded(path, content, size) {
  size = Math.round(size)
  // Readable head, random tail: a document of a realistic size.
  ensure(path)
  const head = Buffer.from(content)
  const fd = openSync(path, 'w')
  writeSync(fd, head)
  let left = Math.max(0, size - head.length)
  const chunk = Buffer.alloc(Math.min(Math.max(left, 1), 4 * MiB))
  while (left > 0) {
    const n = Math.min(left, chunk.length)
    randomFillSync(chunk, 0, n)
    writeSync(fd, chunk, 0, n)
    left -= n
  }
  closeSync(fd)
}

// ---------------------------------------------------------------- files.example.org

const F = join(SRV, FILES)
const pub = (p) => join(F, 'pub', p)

text(pub('documents/release-notes.md'), `# Sample tool 1.4.2 release notes\n\nGenerated sample text for the Zeus pre-alpha.\n\n${lines(40, (i) => `- Change ${i + 1}: an example entry.`)}`)
text(pub('documents/sample-data.csv'), 'id,city,station,trips,duration_s\n' + lines(60000, (i) => `${i + 1},Sample City ${i % 12},Station ${i % 340},${(i * 7) % 97},${300 + ((i * 13) % 2400)}`))
text(pub('documents/inventory.json'), JSON.stringify({ generated: 'sample', items: Array.from({ length: 4000 }, (_, i) => ({ id: i + 1, sku: `SKU-${10000 + i}`, qty: (i * 17) % 300 })) }, null, 1))
text(pub('documents/user-guide.pdf'), pdf('Sample tool user guide', 24))
text(pub('documents/whitepaper-2026.pdf'), pdf('Whitepaper 2026 (sample)', 60))
padded(pub('documents/handbook.epub'), 'PK\u0003\u0004 sample epub container for the Zeus pre-alpha\n', 1.2 * MiB)
padded(pub('documents/quarterly-report.docx'), 'PK\u0003\u0004 sample docx\n', 900 * KiB)
padded(pub('documents/budget.xlsx'), 'PK\u0003\u0004 sample xlsx\n', 420 * KiB)
padded(pub('documents/slides.pptx'), 'PK\u0003\u0004 sample pptx\n', 6 * MiB)
text(pub('documents/notes.txt'), lines(500, (i) => `Line ${i + 1} of a plain sample text file.`))

text(pub('media/diagram.svg'), '<svg xmlns="http://www.w3.org/2000/svg" width="320" height="180"><rect width="320" height="180" fill="#eef"/><text x="20" y="95" font-family="sans-serif" font-size="20">Sample diagram</text></svg>\n')
random(pub('media/wallpaper.png'), 2.5 * MiB)
random(pub('media/photo-set.jpg'), 820 * KiB)
random(pub('media/podcast-episode-12.mp3'), 14 * MiB)
random(pub('media/lecture-01.mp4'), 48 * MiB)
random(pub('media/lecture-02.mp4'), 96 * MiB)

random(pub('software/tool-setup-1.4.2.exe'), 24 * MiB)
random(pub('software/tool-1.4.2.msi'), 26 * MiB)
if (!existsSync(pub('software/tool-1.4.2-linux-x64.tar.gz'))) {
  const stage = join(SRV, '.stage', 'tool-1.4.2')
  random(join(stage, 'bin/tool'), 18 * MiB)
  text(join(stage, 'README.txt'), 'Sample tool 1.4.2 for Linux. Sample content for the Zeus pre-alpha.\n')
  ensure(pub('software/x'))
  execFileSync('tar', ['-czf', pub('software/tool-1.4.2-linux-x64.tar.gz'), '-C', join(SRV, '.stage'), 'tool-1.4.2'])
  execFileSync('rm', ['-rf', join(SRV, '.stage')])
}

random(pub('archives/dataset-2026.zip'), 128 * MiB)
random(pub('archives/backup-small.7z'), 8 * MiB)
random(pub('archives/disk-image.iso'), 256 * MiB)

// Behaviours: slow, no range support, flaky, and paths that always fail (served by nginx rules, see nginx.conf).
random(join(F, 'slow/big-slow.bin'), 32 * MiB)
random(join(F, 'slow/slow-video.mp4'), 12 * MiB)
random(join(F, 'no-range/no-resume.bin'), 16 * MiB)
random(join(F, 'flaky/flaky-archive.zip'), 10 * MiB)

const sums = ['tool-setup-1.4.2.exe', 'tool-1.4.2.msi', 'tool-1.4.2-linux-x64.tar.gz']
  .map((n) => `${createHash('sha256').update(readFileSync(pub(`software/${n}`))).digest('hex')}  ${n}`)
  .join('\n')
text(pub('software/checksums.txt'), sums + '\n')

text(join(F, 'index.html'), `<!doctype html><meta charset="utf-8"><title>files.example.org</title><h1>files.example.org</h1><p>Sample files for the Zeus pre-alpha.</p><ul><li><a href="/pub/">/pub/</a></li><li><a href="/slow/">/slow/</a> (served at 512 KiB/s)</li><li><a href="/no-range/">/no-range/</a> (no resume)</li><li><a href="/flaky/">/flaky/</a> (busy half the time)</li></ul>\n`)

// ---------------------------------------------------------------- docs.example.org: a small site for the site grabber

const D = join(SRV, DOCS)
const page = (title, body) => `<!doctype html><meta charset="utf-8"><title>${title}</title><h1>${title}</h1>${body}\n`
const link = (href, label) => `<li><a href="${href}">${label}</a></li>`
text(join(D, 'index.html'), page('Sample documentation', `<p>A sample site for the Zeus pre-alpha site grabber.</p><ul>${link('/guides/', 'Guides')}${link('/releases/', 'Releases')}${link('/papers/', 'Papers')}${link('/blog/', 'Blog')}${link('https://www.example.com/', 'Elsewhere')}</ul>`))
text(join(D, 'guides/index.html'), page('Guides', `<ul>${link('/guides/getting-started.pdf', 'Getting started')}${link('/guides/admin-guide.pdf', 'Admin guide')}${link('/guides/advanced/', 'Advanced')}${link(`http://${FILES}/pub/documents/user-guide.pdf`, 'User guide (file host)')}</ul>`))
text(join(D, 'guides/advanced/index.html'), page('Advanced guides', `<ul>${link('/guides/advanced/tuning.pdf', 'Tuning')}${link('/guides/advanced/examples.zip', 'Examples')}${link('/guides/advanced/reference.epub', 'Reference')}</ul>`))
text(join(D, 'releases/index.html'), page('Releases', `<ul>${link('/releases/changelog.md', 'Changelog')}${link(`http://${FILES}/pub/software/tool-setup-1.4.2.exe`, 'Windows installer')}${link(`http://${FILES}/pub/software/tool-1.4.2-linux-x64.tar.gz`, 'Linux tarball')}${link(`http://${FILES}/pub/software/checksums.txt`, 'Checksums')}</ul>`))
text(join(D, 'papers/index.html'), page('Papers', `<ul>${link('/papers/paper-2024.pdf', '2024')}${link('/papers/paper-2025.pdf', '2025')}${link('/papers/paper-2026.pdf', '2026')}${link('/papers/dataset.csv', 'Dataset')}${link('/papers/poster.png', 'Poster')}</ul>`))
text(join(D, 'blog/index.html'), page('Blog', `<ul>${link('/blog/post-1.html', 'First post')}${link('/blog/cover.jpg', 'Cover image')}</ul>`))
text(join(D, 'blog/post-1.html'), page('First post', `<p>Sample post.</p><ul>${link('/blog/attachment.pdf', 'Attachment')}</ul>`))
text(join(D, 'guides/getting-started.pdf'), pdf('Getting started (sample)', 8))
text(join(D, 'guides/admin-guide.pdf'), pdf('Admin guide (sample)', 30))
text(join(D, 'guides/advanced/tuning.pdf'), pdf('Tuning (sample)', 12))
random(join(D, 'guides/advanced/examples.zip'), 3 * MiB)
padded(join(D, 'guides/advanced/reference.epub'), 'PK\u0003\u0004 sample epub\n', 700 * KiB)
text(join(D, 'releases/changelog.md'), `# Changelog (sample)\n\n${lines(30, (i) => `- 1.${Math.floor(i / 10)}.${i % 10}: sample entry`)}`)
for (const y of [2024, 2025, 2026]) text(join(D, `papers/paper-${y}.pdf`), pdf(`Paper ${y} (sample)`, 16))
text(join(D, 'papers/dataset.csv'), 'x,y\n' + lines(20000, (i) => `${i},${(i * 31) % 1000}`))
random(join(D, 'papers/poster.png'), 1.6 * MiB)
random(join(D, 'blog/cover.jpg'), 400 * KiB)
text(join(D, 'blog/attachment.pdf'), pdf('Attachment (sample)', 2))

// ---------------------------------------------------------------- the catalogue

const sizes = {}
const walk = (host, dir) => {
  for (const e of readdirSync(dir, { withFileTypes: true })) {
    const full = join(dir, e.name)
    if (e.isDirectory()) walk(host, full)
    else if (e.isFile()) sizes[`${host}/${relative(join(SRV, host), full).split('\\').join('/')}`] = statSync(full).size
  }
}
walk(FILES, F)
walk(DOCS, D)
for (const [k, v] of Object.entries(sizes)) if (k.startsWith(`${FILES}/pub/`)) sizes[k.replace(FILES, MIRROR)] = v

const u = (path) => `http://${FILES}${path}`
const samples = [
  { url: u('/pub/documents/user-guide.pdf'), note: 'PDF, 24 pages' },
  { url: u('/pub/documents/sample-data.csv'), note: 'CSV, about 2 MB' },
  { url: u('/pub/documents/slides.pptx'), note: '6 MiB' },
  { url: u('/pub/media/lecture-01.mp4'), note: '48 MiB video' },
  { url: u('/pub/media/podcast-episode-12.mp3'), note: '14 MiB audio' },
  { url: u('/pub/software/tool-setup-1.4.2.exe'), note: 'Executable: quarantined on completion' },
  { url: u('/pub/software/tool-1.4.2-linux-x64.tar.gz'), note: 'Real gzip tarball' },
  { url: u('/pub/archives/dataset-2026.zip'), note: '128 MiB' },
  { url: u('/slow/big-slow.bin'), note: 'Slow: 512 KiB/s, pause and resume it' },
  { url: u('/no-range/no-resume.bin'), note: 'Server without range support' },
  { url: u('/flaky/flaky-archive.zip'), note: 'Busy half the time (503, Retry-After)' },
  { url: u('/expired/report.pdf?sig=sample&expires=1'), note: 'Signed link that expired (403)' },
  { url: u('/gone/old-release.zip'), note: 'Removed file (410)' },
  { url: u('/broken/file.bin'), note: 'Server error (500)' },
  { url: `http://${MIRROR}/pub/archives/backup-small.7z`, note: 'Mirror host' },
  { url: `http://${DOCS}/`, note: 'Sample site for the site grabber' },
]
const catalogue = { generated: new Date().toISOString(), hosts: [FILES, DOCS, MIRROR], sizes, samples }
text(join(SRV, 'catalogue.json'), JSON.stringify(catalogue, null, 1))
const total = Object.entries(sizes).filter(([k]) => !k.startsWith(MIRROR)).reduce((n, [, v]) => n + v, 0)
console.log(`fixtures ready: ${Object.keys(sizes).length} entries, ${(total / MiB).toFixed(0)} MiB on disk`)
