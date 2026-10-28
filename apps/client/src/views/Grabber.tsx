// The link grabber: paste many links or a whole page, see them grouped into packages by host, check which are online,
// set names, folders and categories, then review and start. The two steps match the desktop client's real flow
// (apps/desktop/README.md): "Preview downloads" resolves destinations without creating jobs, and "Start reviewed
// downloads" starts exactly that reviewed batch. At most 50 unique http(s) links and 64 KiB of input.
//
// In the desktop app the online check is a real probe through zeusd (size, the name the server gives, whether byte
// ranges work), the review is zeusd's dry run, a file that is already in the list is offered as Skip, Download again or
// Replace, and an expected SHA-256 can be set per link.

import { useEffect, useMemo, useState } from 'preact/hooks'

import type { GrabberLink, PlanRow, Review } from '../backend/store'
import { defaultDir, grabberAdd, grabberCheck, grabberPreview, grabberRemove, grabberStart, grabberUpdate, grabberUpdateMany, useStore } from '../backend/store'
import { CATEGORIES } from '../model/categories.ts'
import { folderProblem } from '../model/grab.ts'
import type { DupeChoice, Overwrite } from '../model/grab.ts'
import { MAX_URLS, nameProblem, packagesByHost } from '../model/links.ts'
import type { CategoryId } from '../model/types.ts'
import { fmtBytes } from '../model/units.ts'
import { HOSTED, LIVE, NATIVE } from '../backend/live'
import { SEP, baseOf, parentOf } from '../lib/paths.ts'
import { href } from '../lib/router'
import { Chip, Empty, Notice, PageHead, Provenance } from '../ui/components'
import { Icon } from '../ui/icons'
import { MediaGrabber } from './MediaGrabber'
import '../styles/grabber.css'

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

const OVERWRITE: [Overwrite, string][] = [
  ['rename', 'Keep both: add (1), (2)…'],
  ['fail', 'Stop with an error'],
  ['replace', 'Replace (the old file goes to the trash)'],
]

const DUPE: [DupeChoice, string][] = [
  ['skip', 'Skip'],
  ['again', 'Download again'],
  ['replace', 'Replace the old file'],
]

/** What zeusd's dry run says about one reviewed link. */
function planNote(l: GrabberLink, plan: PlanRow | undefined, collides: boolean) {
  const notes: preact.ComponentChildren[] = []
  if (plan?.error_code) notes.push(<Chip tone="bad">{plan.error_message ?? plan.error_code}</Chip>)
  if (plan?.filename && plan.filename !== l.name) notes.push(<Chip tone="warn">renamed: a file has this name</Chip>)
  else if (!NATIVE && collides) notes.push(<Chip tone="warn">renamed: a file has this name</Chip>)
  if (l.dupe && l.dupeChoice === 'replace') notes.push(<Chip tone="warn">replaces the file on disk</Chip>)
  if (l.dupe && l.dupeChoice === 'again') notes.push(<Chip>downloaded again</Chip>)
  if (plan?.approval_needed || l.check === 'needs_login') notes.push(<Chip tone="warn">asks for a login first</Chip>)
  for (const w of plan?.warnings ?? []) notes.push(<Chip tone="warn">{w}</Chip>)
  if (!notes.length && l.check === 'unchecked') notes.push(<Chip>not checked</Chip>)
  return notes
}

export function Grabber() {
  const links = useStore((s) => s.grabber)
  const queues = useStore((s) => s.queues.filter((q) => q.kind === 'download'))
  const roots = useStore((s) => s.settings.saveTo.writeRoots)
  const defaultOverwrite = useStore((s) => s.settings.downloads.duplicates)
  const [text, setText] = useState('')
  const [summary, setSummary] = useState<ReturnType<typeof grabberAdd> | null>(null)
  const [review, setReview] = useState<Review | null>(null)
  const [reviewErr, setReviewErr] = useState<string | null>(null)
  const startNow = useStore((s) => s.settings.downloads.startImmediately)
  const defaultQueue = useStore((s) => s.settings.scheduler.defaultQueue)
  const [queue, setQueue] = useState<string>(startNow ? 'now' : defaultQueue)
  const [overwrite, setOverwrite] = useState<Overwrite | null>(null)
  const [collapsed, setCollapsed] = useState<string[]>([])
  const [started, setStarted] = useState<{ n: number; skipped: number; left: number } | null>(null)

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
  const dupes = selected.filter((l) => l.dupe)
  const checking = links.filter((l) => l.check === 'checking').length
  const changed = () => {
    setReview(null)
    setReviewErr(null)
  }
  const ci = SEP() === '\\' || !NATIVE
  const badFolders = NATIVE ? selected.filter((l) => folderProblem(l.dir, roots, ci)) : []

  const liveReview = useStore((s) => (review ? s.reviews.find((r) => r.id === review.id) : undefined))
  const remote = liveReview?.remote
  const plan = (i: number): PlanRow | undefined => remote?.plan?.find((p) => p.index === i)
  const preview = () => {
    const r = grabberPreview(sel, { overwrite: overwrite ?? defaultOverwrite })
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
    const left = links.filter((l) => !review.links.some((x) => x.id === l.id)).length
    const r = grabberStart(review.id, queue === 'now' ? null : queue)
    if (!r.ok) return setReviewErr(r.reason)
    setStarted({ n: r.ids.length || review.links.length, skipped: review.skippedDuplicates ?? 0, left })
    setReview(null)
  }

  return (
    <div class="page">
      <PageHead title="Link grabber" sub="Paste links or a whole page. Nothing downloads until you review the batch and start it." />
      {NATIVE ? <MediaGrabber /> : null}
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
          {NATIVE ? null : (
            <button type="button" class="btn small ghost" onClick={() => add(HOSTED ? SAMPLE_HOSTED : SAMPLE)}>
              Add sample links
            </button>
          )}
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
            <button type="button" class="btn small" disabled={checking > 0} onClick={() => grabberCheck(links.filter((l) => l.check === 'unchecked' || l.selected).map((l) => l.id))}>
              <Icon name="refresh" /> {checking ? `Checking… ${links.length - checking} of ${links.length}` : 'Check online status'}
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
                  if (c) for (const l of selected) grabberUpdate(l.id, { category: c, ...(l.dirEdited ? {} : { dir: defaultDir(c) }) })
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
                        class={`mono pkgdir ${NATIVE && folderProblem(p.items[0]?.dir ?? '', roots, ci) ? 'bad' : ''}`}
                        aria-label={`Folder for ${p.host}`}
                        value={p.items[0]?.dir ?? ''}
                        title={NATIVE ? (folderProblem(p.items[0]?.dir ?? '', roots, ci) ?? 'Inside a download folder') : undefined}
                        onChange={(e) => (grabberUpdateMany(p.items.map((l) => l.id), { dir: (e.target as HTMLInputElement).value, dirEdited: true }), changed())}
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
                          {NATIVE ? <th>Expected SHA-256</th> : null}
                          <th>Address</th>
                        </tr>
                      </thead>
                      <tbody>
                        {p.items.map((l) => {
                          const problem = nameProblem(l.name)
                          const shaBad = !!l.expectedSha256 && !/^[0-9a-f]{64}$/.test(l.expectedSha256)
                          return (
                            <>
                              <tr aria-selected={l.selected} class={l.check === 'offline' ? 'rowbad' : undefined}>
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
                                  <select
                                    class="cellselect"
                                    aria-label="Category"
                                    value={l.category}
                                    onChange={(e) => {
                                      const c = (e.target as HTMLSelectElement).value as CategoryId
                                      grabberUpdate(l.id, { category: c, ...(l.dirEdited ? {} : { dir: defaultDir(c) }) })
                                      changed()
                                    }}
                                  >
                                    {CATEGORIES.map((c) => (
                                      <option value={c.id}>{c.label}</option>
                                    ))}
                                  </select>
                                </td>
                                {NATIVE ? (
                                  <td>
                                    <input
                                      class={`cellinput mono small ${shaBad ? 'bad' : ''}`}
                                      aria-label={`Expected SHA-256 of ${l.name}`}
                                      placeholder="optional, 64 hex digits"
                                      value={l.expectedSha256 ?? ''}
                                      title={shaBad ? 'A SHA-256 is 64 hexadecimal digits.' : 'The hash the publisher gives. zeusd refuses the file if its bytes differ.'}
                                      onChange={(e) => {
                                        const v = (e.target as HTMLInputElement).value.trim().toLowerCase()
                                        grabberUpdate(l.id, v ? { expectedSha256: v } : { expectedSha256: undefined as unknown as string })
                                        changed()
                                      }}
                                    />
                                  </td>
                                ) : null}
                                <td class="mono small addr" title={l.queryWithheld ? 'The query string is withheld from display' : undefined}>
                                  {l.display.replace(/^https?:\/\//, '')}
                                  {l.queryWithheld ? ' ?…' : ''}
                                </td>
                              </tr>
                              {l.note || l.dupe ? (
                                <tr class="gnote">
                                  <td />
                                  <td colSpan={NATIVE ? 7 : 6}>
                                    {l.dupe ? (
                                      <span class="dupe">
                                        <Chip tone="warn">{l.dupe.where}</Chip> <span class="muted small">as {l.dupe.name}.</span>{' '}
                                        <select
                                          class="cellselect"
                                          aria-label={`What to do with the duplicate ${l.name}`}
                                          value={l.dupeChoice ?? 'skip'}
                                          onChange={(e) => {
                                            const choice = (e.target as HTMLSelectElement).value as DupeChoice
                                            grabberUpdate(l.id, { dupeChoice: choice, selected: choice !== 'skip' })
                                            changed()
                                          }}
                                        >
                                          {DUPE.map(([v, label]) => (
                                            <option value={v}>{label}</option>
                                          ))}
                                        </select>{' '}
                                      </span>
                                    ) : null}
                                    {l.note ? <span class={`muted small ${l.check === 'offline' ? 'gnote-bad' : ''}`}>{l.note}</span> : null}
                                  </td>
                                </tr>
                              ) : null}
                            </>
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
              {dupes.length ? ` · ${dupes.length} already in your list` : ''}
            </span>
            <span class="spacer" />
            {NATIVE ? (
              <label class="inline small">
                If a file with the name exists
                <select aria-label="If a file with the name exists" value={overwrite ?? defaultOverwrite} onChange={(e) => (setOverwrite((e.target as HTMLSelectElement).value as Overwrite), changed())}>
                  {OVERWRITE.map(([v, label]) => (
                    <option value={v}>{label}</option>
                  ))}
                </select>
              </label>
            ) : null}
            {!review ? (
              <button type="button" class="btn primary" disabled={!selected.length || badFolders.length > 0} title={badFolders.length ? 'A folder is outside the download folders' : undefined} onClick={preview}>
                <Icon name="eye" /> Preview downloads
              </button>
            ) : null}
          </div>
          {reviewErr ? <Notice kind="error">{reviewErr}</Notice> : null}
          {badFolders.length ? <Notice kind="warn">{folderProblem(badFolders[0]?.dir ?? '', roots, ci)}</Notice> : null}
          {review ? (
            <section class="review">
              <h2>Review</h2>
              <p class="muted small">
                These {review.links.length} files would be created exactly as listed. No job exists yet. The review holds the original addresses for ten minutes; changing anything above discards it.
                {review.skippedDuplicates ? ` ${review.skippedDuplicates} duplicate${review.skippedDuplicates === 1 ? '' : 's'} left out because you chose Skip.` : ''}
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
                    {review.links.map((l, i) => {
                      const p = plan(i)
                      const where = p?.destination ? parentOf(p.destination) : review.destinations[i]?.dir
                      return (
                        <tr>
                          <td>{p?.filename ?? (p?.destination ? baseOf(p.destination) : review.destinations[i]?.name)}</td>
                          <td class="mono small">{where}</td>
                          <td class="num">{fmtBytes(l.size ?? p?.size ?? null)}</td>
                          <td>{planNote(l, p, !!review.destinations[i]?.collides)}</td>
                        </tr>
                      )
                    })}
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
                  <button type="button" class="btn primary" disabled={remote?.state === 'refused'} onClick={start}>
                    <Icon name="download" /> Start reviewed downloads
                  </button>
                </span>
              </div>
            </section>
          ) : null}
        </>
      ) : (
        <Empty>
          {NATIVE ? (
            <>No links yet. Paste some above: one per line, or the HTML of a page.</>
          ) : (
            <>
              No links yet. Paste some above, or press <strong>Add sample links</strong> to see how packages, duplicates and refusals look.
            </>
          )}
        </Empty>
      )}
      {started !== null ? (
        <Notice kind="ok">
          Started {started.n} download{started.n === 1 ? '' : 's'}.
          {started.skipped ? ` ${started.skipped} duplicate${started.skipped === 1 ? '' : 's'} skipped.` : ''}
          {started.left ? ` ${started.left} link${started.left === 1 ? '' : 's'} not started stay in the list.` : ''} <a href={href('downloads', { view: 'unfinished' })}>See them in Downloads</a>
        </Notice>
      ) : null}
      <Provenance>
        {NATIVE
          ? "The online check is a probe through zeusd: the size, the name the server gives and whether byte ranges work. Preview is zeusd's dry run, and Start enqueues exactly that reviewed batch once, with a retry key. Nothing is fetched by this window itself."
          : LIVE
            ? `Preview asks zeusd for a dry run through zeus-web-bridge, and Start enqueues exactly that reviewed batch. ${HOSTED ? 'Online checks are HEAD requests from the pre-alpha service to the sample hosts; nothing reaches the internet.' : 'Online checks are dry runs in your zeusd.'}`
            : "Preview and start match the desktop client's real two-step review against zeusd. Online checks, sizes and packages here are answered from sample data."}
      </Provenance>
    </div>
  )
}
