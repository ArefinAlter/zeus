// The link grabber: paste many links or a whole page, see them grouped into packages by host, check which are online,
// set names, folders and categories, then review and start. The two steps match the desktop client's real flow
// (apps/desktop/README.md): "Preview downloads" resolves destinations without creating jobs, and "Start reviewed
// downloads" starts exactly that reviewed batch. At most 50 unique http(s) links and 64 KiB of input.

import { useEffect, useMemo, useState } from 'preact/hooks'

import type { GrabberLink, Review } from '../backend/store'
import { grabberAdd, grabberCheck, grabberPreview, grabberRemove, grabberStart, grabberUpdate, grabberUpdateMany, useStore } from '../backend/store'
import { CATEGORIES } from '../model/categories.ts'
import { MAX_URLS, nameProblem, packagesByHost } from '../model/links.ts'
import type { CategoryId } from '../model/types.ts'
import { fmtBytes } from '../model/units.ts'
import { HOSTED, LIVE } from '../backend/live'
import { href } from '../lib/router'
import { Chip, Empty, Notice, PageHead, Provenance } from '../ui/components'
import { Icon } from '../ui/icons'

const SAMPLE = [
  'https://papers.example.org/2026/annual-review.pdf',
  'https://papers.example.org/2026/methods-appendix.pdf',
  'https://papers.example.org/2026/annual-review.pdf',
  'https://cdn.example.com/media/keynote-recording.mp4?token=abc123',
  'https://cdn.example.com/media/keynote-slides.pdf',
  'https://mirror.example.net/old-builds/tool-1.0.zip',
  'https://members.example.org/library/handbook.epub',
  'magnet:?xt=urn:btih:0000',
  'ftp://files.example.net/legacy.tar',
].join('\n')

// The hosted pre-alpha downloads only from its sample hosts; the last addresses show refusals.
const SAMPLE_HOSTED = [
  'http://files.example.org/pub/documents/user-guide.pdf',
  'http://files.example.org/pub/documents/sample-data.csv',
  'http://files.example.org/pub/documents/user-guide.pdf',
  'http://files.example.org/pub/media/podcast-episode-12.mp3',
  'http://files.example.org/slow/big-slow.bin',
  'http://mirror.example.net/pub/archives/backup-small.7z',
  'http://files.example.org/gone/old-release.zip',
  'magnet:?xt=urn:btih:0000',
].join('\n')

function checkChip(c: GrabberLink['check']) {
  switch (c) {
    case 'online':
      return <Chip tone="ok">online</Chip>
    case 'offline':
      return <Chip tone="bad">offline</Chip>
    case 'needs_login':
      return <Chip tone="warn">needs login</Chip>
    case 'checking':
      return <Chip>checking…</Chip>
    default:
      return <Chip>not checked</Chip>
  }
}

export function Grabber() {
  const links = useStore((s) => s.grabber)
  const queues = useStore((s) => s.queues.filter((q) => q.kind === 'download'))
  const [text, setText] = useState('')
  const [summary, setSummary] = useState<ReturnType<typeof grabberAdd> | null>(null)
  const [review, setReview] = useState<Review | null>(null)
  const [reviewErr, setReviewErr] = useState<string | null>(null)
  const [queue, setQueue] = useState<string>('now')
  const [collapsed, setCollapsed] = useState<string[]>([])
  const [started, setStarted] = useState<number | null>(null)

  const add = (t: string) => {
    if (!t.trim()) return
    setSummary(grabberAdd(t))
    setText('')
    setReview(null)
    setStarted(null)
  }
  useEffect(() => {
    const on = (e: Event) => add(String((e as CustomEvent).detail ?? ''))
    window.addEventListener('zeus:grabber-paste', on)
    return () => window.removeEventListener('zeus:grabber-paste', on)
  }, [])

  const packages = useMemo(() => packagesByHost(links.map((l) => ({ ...l, host: l.pkg }))), [links])
  const selected = links.filter((l) => l.selected)
  const sel = selected.map((l) => l.id)
  const total = selected.reduce((n, l) => n + (l.size ?? 0), 0)
  const changed = () => {
    setReview(null)
    setReviewErr(null)
  }

  const liveReview = useStore((s) => (review ? s.reviews.find((r) => r.id === review.id) : undefined))
  const remote = liveReview?.remote
  const preview = () => {
    const r = grabberPreview(sel)
    if (typeof r === 'string') {
      setReviewErr(r)
      setReview(null)
    } else {
      setReview(r)
      setReviewErr(null)
    }
  }
  const start = () => {
    if (!review) return
    const r = grabberStart(review.id, queue === 'now' ? null : queue)
    if (!r.ok) return setReviewErr(r.reason)
    setStarted(r.ids.length || review.links.length)
    setReview(null)
  }

  return (
    <div class="page">
      <PageHead title="Link grabber" sub="Paste links or a whole page. Nothing downloads until you review the batch and start it." />
      <div class="pastebox">
        <textarea
          aria-label="Links or a page to grab from"
          rows={4}
          class="mono"
          placeholder={'One link per line, or paste the HTML of a page.\nhttps://files.example.org/archive.zip'}
          value={text}
          onInput={(e) => setText((e.target as HTMLTextAreaElement).value)}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && (e.ctrlKey || e.metaKey)) add(text)
          }}
        />
        <div class="toolbar tight">
          <button type="button" class="btn primary small" disabled={!text.trim()} onClick={() => add(text)}>
            <Icon name="plus" /> Add links
          </button>
          <button
            type="button"
            class="btn small"
            onClick={async () => {
              try {
                const t = await navigator.clipboard.readText()
                add(t)
              } catch {
                setSummary({ links: [], duplicates: 0, refused: [{ text: 'clipboard', reason: 'The clipboard could not be read here. Paste into the box instead.' }], overLimit: 0, tooLarge: false })
              }
            }}
          >
            <Icon name="copy" /> Paste from clipboard
          </button>
          <button type="button" class="btn small ghost" onClick={() => add(HOSTED ? SAMPLE_HOSTED : SAMPLE)}>
            Add sample links
          </button>
          <span class="spacer" />
          <span class="muted small">
            {links.length} of {MAX_URLS} links · Ctrl+Enter adds
          </span>
        </div>
      </div>
      {summary ? (
        <div class="intake">
          <span>
            Added {summary.links.length} link{summary.links.length === 1 ? '' : 's'}
            {summary.duplicates ? `, ${summary.duplicates} exact duplicate${summary.duplicates === 1 ? '' : 's'} dropped` : ''}
            {summary.overLimit ? `, ${summary.overLimit} over the ${MAX_URLS}-link limit left out` : ''}.
          </span>
          {summary.tooLarge ? <Chip tone="warn">input cut at 64 KiB</Chip> : null}
          {summary.refused.length ? (
            <ul class="refused">
              {summary.refused.map((r) => (
                <li>
                  <span class="mono small">{r.text}</span> {r.reason}
                </li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}

      {links.length ? (
        <>
          <div class="toolbar">
            <button type="button" class="btn small" onClick={() => grabberCheck(links.filter((l) => l.check === 'unchecked' || l.selected).map((l) => l.id))}>
              <Icon name="refresh" /> Check online status
            </button>
            <button type="button" class="btn small ghost" onClick={() => (grabberUpdateMany(links.map((l) => l.id), { selected: true }), changed())}>
              Select all
            </button>
            <button type="button" class="btn small ghost" onClick={() => (grabberUpdateMany(links.map((l) => l.id), { selected: false }), changed())}>
              Select none
            </button>
            <button type="button" class="btn small ghost" disabled={!links.some((l) => l.check === 'offline')} onClick={() => (grabberRemove(links.filter((l) => l.check === 'offline').map((l) => l.id)), changed())}>
              Remove offline
            </button>
            <button type="button" class="btn small ghost" disabled={!sel.length} onClick={() => (grabberRemove(sel), changed())}>
              <Icon name="trash" /> Remove selected
            </button>
            <span class="spacer" />
            <label class="inline small">
              Category for selected
              <select
                aria-label="Category for selected"
                disabled={!sel.length}
                value=""
                onChange={(e) => {
                  const c = (e.target as HTMLSelectElement).value as CategoryId
                  if (c) grabberUpdateMany(sel, { category: c })
                  changed()
                }}
              >
                <option value="">Choose…</option>
                {CATEGORIES.map((c) => (
                  <option value={c.id}>{c.label}</option>
                ))}
              </select>
            </label>
          </div>
          <div class="packages">
            {packages.map((p) => {
              const open = !collapsed.includes(p.host)
              const size = p.items.reduce((n, l) => n + (l.size ?? 0), 0)
              const allSel = p.items.every((l) => l.selected)
              return (
                <section class="package">
                  <header>
                    <button type="button" class="icon-btn small" aria-expanded={open} aria-label={open ? 'Collapse package' : 'Expand package'} onClick={() => setCollapsed((c) => (open ? [...c, p.host] : c.filter((x) => x !== p.host)))}>
                      <Icon name={open ? 'chevron' : 'chevronright'} />
                    </button>
                    <input type="checkbox" aria-label={`Select package ${p.host}`} checked={allSel} onChange={(e) => (grabberUpdateMany(p.items.map((l) => l.id), { selected: (e.target as HTMLInputElement).checked }), changed())} />
                    <Icon name="box" />
                    <input
                      class="pkgname"
                      aria-label="Package name"
                      value={p.host}
                      onChange={(e) => {
                        const v = (e.target as HTMLInputElement).value.trim()
                        if (v) grabberUpdateMany(p.items.map((l) => l.id), { pkg: v })
                      }}
                    />
                    <span class="muted small">
                      {p.items.length} file{p.items.length === 1 ? '' : 's'} · {fmtBytes(size)} · {p.items.filter((l) => l.check === 'online').length} online
                    </span>
                    <span class="spacer" />
                    <label class="inline small">
                      Folder
                      <input
                        class="mono pkgdir"
                        aria-label={`Folder for ${p.host}`}
                        value={p.items[0]?.dir ?? ''}
                        onChange={(e) => (grabberUpdateMany(p.items.map((l) => l.id), { dir: (e.target as HTMLInputElement).value }), changed())}
                      />
                    </label>
                  </header>
                  {open ? (
                    <table class="glinks">
                      <thead>
                        <tr>
                          <th class="ck" />
                          <th>Name</th>
                          <th class="num">Size</th>
                          <th>Type</th>
                          <th>Status</th>
                          <th>Category</th>
                          <th>Address</th>
                        </tr>
                      </thead>
                      <tbody>
                        {p.items.map((l) => {
                          const problem = nameProblem(l.name)
                          return (
                            <tr aria-selected={l.selected}>
                              <td class="ck">
                                <input type="checkbox" aria-label={`Select ${l.name}`} checked={l.selected} onChange={(e) => (grabberUpdate(l.id, { selected: (e.target as HTMLInputElement).checked }), changed())} />
                              </td>
                              <td>
                                <input class={`cellinput ${problem ? 'bad' : ''}`} aria-label="File name" value={l.name} title={problem ?? 'Rename before starting'} onChange={(e) => (grabberUpdate(l.id, { name: (e.target as HTMLInputElement).value }), changed())} />
                              </td>
                              <td class="num">{l.size === null ? <span class="muted">–</span> : fmtBytes(l.size)}</td>
                              <td class="mono small">{l.type}</td>
                              <td>{checkChip(l.check)}</td>
                              <td>
                                <select class="cellselect" aria-label="Category" value={l.category} onChange={(e) => (grabberUpdate(l.id, { category: (e.target as HTMLSelectElement).value as CategoryId }), changed())}>
                                  {CATEGORIES.map((c) => (
                                    <option value={c.id}>{c.label}</option>
                                  ))}
                                </select>
                              </td>
                              <td class="mono small addr" title={l.queryWithheld ? 'The query string is withheld from display' : undefined}>
                                {l.display.replace(/^https?:\/\//, '')}
                                {l.queryWithheld ? ' ?…' : ''}
                              </td>
                            </tr>
                          )
                        })}
                      </tbody>
                    </table>
                  ) : null}
                </section>
              )
            })}
          </div>
          <div class="reviewbar">
            <span>
              <strong>{selected.length}</strong> selected · {fmtBytes(total)}
              {selected.some((l) => l.check === 'needs_login') ? ' · some need a site login; Zeus will ask you first' : ''}
            </span>
            <span class="spacer" />
            {!review ? (
              <button type="button" class="btn primary" disabled={!selected.length} onClick={preview}>
                <Icon name="eye" /> Preview downloads
              </button>
            ) : null}
          </div>
          {reviewErr ? <Notice kind="error">{reviewErr}</Notice> : null}
          {review ? (
            <section class="review">
              <h2>Review</h2>
              <p class="muted small">
                These {review.links.length} files would be created exactly as listed. No job exists yet. The review holds the original addresses for ten minutes; changing anything above discards it.
              </p>
              <div class="table-wrap short">
                <table>
                  <thead>
                    <tr>
                      <th>Saved as</th>
                      <th>Folder</th>
                      <th class="num">Size</th>
                      <th>Note</th>
                    </tr>
                  </thead>
                  <tbody>
                    {review.links.map((l, i) => (
                      <tr>
                        <td>{review.destinations[i]?.name}</td>
                        <td class="mono small">{review.destinations[i]?.dir}</td>
                        <td class="num">{fmtBytes(l.size)}</td>
                        <td>{review.destinations[i]?.collides ? <Chip tone="warn">renamed: a file has this name</Chip> : l.check === 'needs_login' ? <Chip tone="warn">asks for a login first</Chip> : l.check === 'unchecked' ? <Chip>not checked</Chip> : null}</td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
              {remote ? (
                remote.state === 'checking' ? (
                  <Notice kind="info">zeusd is resolving this batch (a dry run: no job is created yet)…</Notice>
                ) : remote.state === 'refused' ? (
                  <Notice kind="error">{remote.reason ?? 'zeusd refused this batch.'}</Notice>
                ) : (
                  <Notice kind="ok">zeusd accepted the dry run. Starting enqueues exactly this batch, once.</Notice>
                )
              ) : null}
              <div class="decide-row split">
                <button type="button" class="btn" onClick={() => setReview(null)}>
                  Back to the links
                </button>
                <span class="inline">
                  <select aria-label="Start now or queue" value={queue} onChange={(e) => setQueue((e.target as HTMLSelectElement).value)}>
                    <option value="now">Start now</option>
                    {queues.map((q) => (
                      <option value={q.id}>Add to {q.name}</option>
                    ))}
                  </select>
                  <button type="button" class="btn primary" onClick={start}>
                    <Icon name="download" /> Start reviewed downloads
                  </button>
                </span>
              </div>
            </section>
          ) : null}
        </>
      ) : (
        <Empty>
          No links yet. Paste some above, or press <strong>Add sample links</strong> to see how packages, duplicates and refusals look.
        </Empty>
      )}
      {started !== null ? (
        <Notice kind="ok">
          Started {started} download{started === 1 ? '' : 's'}. <a href={href('downloads', { view: 'unfinished' })}>See them in Downloads</a>
        </Notice>
      ) : null}
      <Provenance>
        {LIVE
          ? `Preview asks zeusd for a dry run through zeus-web-bridge, and Start enqueues exactly that reviewed batch. ${HOSTED ? 'Online checks are HEAD requests from the pre-alpha service to the sample hosts; nothing reaches the internet.' : 'Online checks are dry runs in your zeusd.'}`
          : "Preview and start match the desktop client's real two-step review against zeusd. Online checks, sizes and packages here are answered from sample data."}
      </Provenance>
    </div>
  )
}
