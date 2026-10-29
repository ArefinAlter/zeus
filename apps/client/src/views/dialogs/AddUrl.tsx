// Add URL: enter an address, see what the server says about it (a probe), choose the category, folder and name, and
// either start now or put it in a queue for later. The probe answers come from sample data in this release.

import { useEffect, useMemo, useRef, useState } from 'preact/hooks'

import { HOSTED, LIVE, NATIVE, SVC } from '../../backend/live'
import type { LinkCheck } from '../../backend/remote'
import { api, svc } from '../../backend/remote'
import { addDownloads, defaultDir, duplicateOf, rememberFolder, sizeFor, useStore } from '../../backend/store'
import { CATEGORIES, categoryOf } from '../../model/categories.ts'
import type { DupeChoice } from '../../model/grab.ts'
import { checkNote, folderProblem, overwriteFor } from '../../model/grab.ts'
import { SEP, joinPath } from '../../lib/paths.ts'
import { intake, nameProblem } from '../../model/links.ts'
import type { CategoryId } from '../../model/types.ts'
import { fmtBytes } from '../../model/units.ts'
import { href, navigate } from '../../lib/router'
import { openDialog } from '../../lib/ui'
import { Chip, Dialog, Notice } from '../../ui/components'
import { Icon } from '../../ui/icons'

export function AddUrl({ initial, onClose }: { initial?: string; onClose: () => void }) {
  const queues = useStore((s) => s.queues.filter((q) => NATIVE || q.kind === 'download'))
  const settings = useStore((s) => s.settings)
  const [url, setUrl] = useState(initial ?? '')
  const [probed, setProbed] = useState<string | null>(null)
  const [probing, setProbing] = useState(false)
  const [name, setName] = useState('')
  const [category, setCategory] = useState<CategoryId>('general')
  const [dir, setDir] = useState('')
  const [desc, setDesc] = useState('')
  const [expected, setExpected] = useState('')
  const [queue, setQueue] = useState(settings.scheduler.defaultQueue)
  const [error, setError] = useState<string | null>(null)
  // The folder was typed (or confirmed) by the person: a name or category change leaves it alone.
  const [dirEdited, setDirEdited] = useState(false)
  const [categoryEdited, setCategoryEdited] = useState(false)
  // "Ask where to save each download" (Settings, Save to): the folder must be confirmed before a download starts.
  const [confirmed, setConfirmed] = useState(false)
  const [dupeChoice, setDupeChoice] = useState<DupeChoice>('skip')
  const input = useRef<HTMLInputElement>(null)
  useEffect(() => input.current?.focus(), [])
  // Live: what the server answered to a HEAD request (hosted: from the pre-alpha service, sample hosts only).
  const [answer, setAnswer] = useState<LinkCheck | null>(null)
  const [samples, setSamples] = useState<{ url: string; note: string }[]>([])
  useEffect(() => {
    if (!HOSTED) return
    api.fixtures().then((f) => setSamples(f.samples), () => setSamples([]))
  }, [])

  const parsed = useMemo(() => intake(url.trim()), [url])
  const link = parsed.links[0]

  useEffect(() => {
    if (link && !name) {
      setName(link.name)
      const c = categoryOf(link.name, settings.categories)
      setCategory(c)
      setDir(defaultDir(c))
    }
  }, [link, name])

  useEffect(() => {
    if (link && !probed && !probing) {
      probe()
    }
  }, [link?.url])

  const dupe = LIVE && link ? duplicateOf(link.url) : undefined
  const roots = settings.saveTo.writeRoots
  const ci = SEP() === '\\' || !NATIVE
  const dirProblem = NATIVE && probed ? folderProblem(dir, roots, ci) : null
  const mustConfirm = NATIVE && settings.saveTo.askEachTime && !confirmed
  const refused = !link && url.trim() ? (parsed.refused[0]?.reason ?? 'Enter an http or https address.') : null
  const many = parsed.links.length > 1

  const probe = () => {
    if (!link) return
    setProbing(true)
    setError(null)
    if (LIVE) {
      const done = (a: LinkCheck | null) => {
        setProbing(false)
        setAnswer(a)
        setProbed(link.url)
        const found = a?.name?.trim() || link.name
        setName(found)
        const c = categoryOf(found, settings.categories)
        setCategory(c)
        setDir(defaultDir(c))
        setDirEdited(false)
        setCategoryEdited(false)
        setConfirmed(false)
        setDupeChoice('skip')
      }
      if (SVC) svc.check([link.url]).then((r) => done(r.items[0] ?? null), (e: unknown) => (setProbing(false), setError(e instanceof Error ? e.message : 'The check failed.')))
      else done(null)
      return
    }
    setTimeout(() => {
      setProbing(false)
      setProbed(link.url)
      setName(link.name)
      const c = categoryOf(link.name, settings.categories)
      setCategory(c)
      setDir(defaultDir(c))
    }, 450)
  }
  const login = !LIVE && !!link && /portal\.|library\.|members\./.test(link.host)
  const size = LIVE ? (answer?.size ?? null) : link ? sizeFor(link.url) : null
  const sha = expected.trim().toLowerCase()
  const shaBad = sha !== '' && !/^[0-9a-f]{64}$/.test(sha)
  const problem = link ? nameProblem(name) : null
  const blocked = !link || !!problem || shaBad || !!dirProblem || mustConfirm || (!!dupe && dupeChoice === 'skip')

  const add = (start: boolean) => {
    if (!link) return
    const res = addDownloads([{ url: link.url, name, dir, queue: start ? null : queue, start, ...(desc.trim() ? { description: desc.trim() } : {}), ...(sha && !shaBad ? { expectedSha256: sha } : {}), ...(login ? { sessionOrigin: new URL(link.url).origin } : {}), ...(NATIVE ? { overwrite: overwriteFor(dupe ? dupeChoice : undefined, settings.downloads.duplicates) } : {}) }])
    if (res.refused.length) return setError(res.refused[0] ?? 'Refused.')
    if (NATIVE) rememberFolder(category, dir)
    if (start && res.ids[0]) {
      openDialog({ kind: 'downloadProgress', jobId: res.ids[0] })
    } else {
      onClose()
    }
  }

  return (
    <Dialog title="Add URL" onClose={onClose}>
      <label class="field">
        Address
        <span class="inline">
          <input
            ref={input}
            class="mono"
            value={url}
            placeholder="https://files.example.org/archive.zip"
            onInput={(e) => {
              setUrl((e.target as HTMLInputElement).value)
              setProbed(null)
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter' && link && !many) probe()
            }}
          />
          <button type="button" class="btn" disabled={!link || many || probing} onClick={probe}>
            {probing ? 'Checking…' : 'Check'}
          </button>
        </span>
      </label>
      {refused ? <p class="field-err">{refused}</p> : null}
      {HOSTED && !url.trim() && samples.length ? (
        <div class="stack-tight">
          <p class="muted small">This server downloads only from its sample hosts. Pick one to try:</p>
          <div class="rows card-rows">
            {samples.map((x) => (
              <button type="button" class="row" onClick={() => (setUrl(x.url), setProbed(null))}>
                <span class="text">
                  <span class="title mono small">{x.url.replace(/^http:\/\//, '')}</span>
                  <span class="desc">{x.note}</span>
                </span>
              </button>
            ))}
          </div>
        </div>
      ) : null}
      {many ? (
        <Notice>
          That is {parsed.links.length} addresses. The link grabber reviews many at once.{' '}
          <button
            type="button"
            class="btn small"
            onClick={() => {
              onClose()
              navigate(href('grabber', { paste: '1' }))
              setTimeout(() => window.dispatchEvent(new CustomEvent('zeus:grabber-paste', { detail: url })), 50)
            }}
          >
            Open the link grabber
          </button>
        </Notice>
      ) : null}
      {link && link.queryWithheld ? <p class="muted small">The address has a query string. Zeus keeps it for the request and never shows or logs it.</p> : null}
      {link ? (
        <>
          <div class="probe">
            <Icon name={probing ? 'refresh' : 'check'} />
            <span>
              {probing && !answer
                ? 'Connecting to server… Zeus will begin download immediately.'
                : LIVE
                ? answer
                  ? answer.check === 'online'
                    ? `${answer.status} · ${size === null ? 'size unknown' : fmtBytes(size)}${answer.redirects ? ` · ${answer.redirects} redirect${answer.redirects === 1 ? '' : 's'}` : ''}${answer.content_type ? ` · ${answer.content_type.split(';')[0]}` : ''}`
                    : answer.check === 'needs_login'
                      ? 'The server wants a sign-in. Zeus lends a browser login only through the extension, after you approve.'
                      : checkNote(answer)
                  : 'Direct high-performance Range download ready.'
                : login
                  ? 'The site answered with a sign-in page: this file needs your login.'
                  : `206 Partial Content · ${fmtBytes(size)} · resumable`}
            </span>
            {probing && !answer ? (
              <Chip tone="info">checking</Chip>
            ) : LIVE ? (
              answer ? <Chip tone={answer.check === 'online' ? 'ok' : 'bad'}>{answer.check}</Chip> : <Chip tone="ok">ready</Chip>
            ) : login ? (
              <Chip tone="warn">needs a site login</Chip>
            ) : (
              <Chip tone="ok">ranges verified</Chip>
            )}
          </div>
          {NATIVE && answer?.check === 'online' ? (
            <p class="muted small">
              {answer.resumable === 'verified' ? 'The server honours byte ranges: Zeus downloads in parallel parts and can resume.' : answer.resumable === 'unsupported' ? 'The server ignores byte ranges: this file downloads in one piece and starts over if interrupted.' : 'Whether the server honours byte ranges is learned when the download starts.'}
              {answer.name && answer.name_source === 'content_disposition' ? ' The file name comes from the server (Content-Disposition).' : ''}
            </p>
          ) : null}
          {dupe ? (
            <Notice kind="warn">
              This file is {dupe.where}, as {dupe.name}.{' '}
              <select aria-label="What to do with the duplicate" value={dupeChoice} onChange={(e) => setDupeChoice((e.target as HTMLSelectElement).value as DupeChoice)}>
                <option value="skip">Skip it</option>
                <option value="again">Download again, keep both</option>
                <option value="replace">Replace the file on disk (the old one goes to the trash)</option>
              </select>
            </Notice>
          ) : null}
          {login ? <Notice kind="warn">Zeus will ask for your approval before it uses your login for {link.host}. The session comes from the Zeus extension, in memory only; Zeus never reads your browser's cookie store.</Notice> : null}
          <div class="fields">
            <label class="field">
              Category
              <select
                value={category}
                onChange={(e) => {
                  const c = (e.target as HTMLSelectElement).value as CategoryId
                  setCategory(c)
                  setCategoryEdited(true)
                  if (!dirEdited) setDir(defaultDir(c))
                }}
              >
                {CATEGORIES.map((c) => (
                  <option value={c.id}>{c.label}</option>
                ))}
              </select>
            </label>
            <label class="field">
              Folder
              <input
                class={`mono ${dirProblem ? 'bad' : ''}`}
                list="add-url-folders"
                value={dir}
                onInput={(e) => {
                  setDir((e.target as HTMLInputElement).value)
                  setDirEdited(true)
                  setConfirmed(true)
                }}
              />
              <datalist id="add-url-folders">
                {[...roots, ...CATEGORIES.filter((c) => c.id !== 'general').flatMap((c) => roots.slice(0, 1).map((r) => joinPath(r, c.folder)))].map((d) => (
                  <option value={d} />
                ))}
              </datalist>
            </label>
            <label class="field wide">
              Save as
              <input
                value={name}
                onInput={(e) => {
                  const v = (e.target as HTMLInputElement).value
                  setName(v)
                  if (!categoryEdited) {
                    const c = categoryOf(v, settings.categories)
                    if (c !== category) {
                      setCategory(c)
                      if (!dirEdited) setDir(defaultDir(c))
                    }
                  }
                }}
              />
            </label>
            {problem ? <p class="field-err wide">{problem}</p> : null}
            {dirProblem ? <p class="field-err wide">{dirProblem}</p> : null}
            {mustConfirm ? (
              <p class="wide">
                <Notice kind="info">
                  You asked Zeus to ask where each download is saved. It will be saved in <span class="mono">{dir}</span>.{' '}
                  <button type="button" class="btn small" disabled={!!dirProblem} onClick={() => setConfirmed(true)}>
                    Save here
                  </button>
                </Notice>
              </p>
            ) : null}
            {LIVE && !NATIVE ? null : (
              <label class="field wide">
                Expected SHA-256 <span class="muted">optional, the hash the publisher gives</span>
                <input class="mono" value={expected} placeholder="64 hex digits from the publisher" onInput={(e) => setExpected((e.target as HTMLInputElement).value)} />
              </label>
            )}
            {shaBad ? <p class="field-err wide">A SHA-256 is 64 hexadecimal digits.</p> : null}
            <label class="field wide">
              Description <span class="muted">optional</span>
              <input value={desc} onInput={(e) => setDesc((e.target as HTMLInputElement).value)} />
            </label>
          </div>
          {error ? <Notice kind="error">{error}</Notice> : null}
          <div class="decide-row split">
            <span class="inline">
              <select aria-label="Queue for later" value={queue} onChange={(e) => setQueue((e.target as HTMLSelectElement).value)}>
                {queues.map((q) => (
                  <option value={q.id}>{q.name}</option>
                ))}
              </select>
              <button type="button" class="btn" disabled={blocked} onClick={() => add(false)}>
                Download later
              </button>
            </span>
            <button type="button" class="btn primary" disabled={blocked} onClick={() => add(true)}>
              <Icon name="download" /> Start download
            </button>
          </div>
        </>
      ) : null}
    </Dialog>
  )
}
