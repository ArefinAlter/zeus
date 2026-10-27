// The shell: a 272 px tinted sidebar with the sections, categories, queues and agent threads, a 40 px top bar that is
// the window's title bar in the desktop app, the page, a status bar with the global speed, and the command palette,
// toasts and global dialogs. T3 Code's layout, Zeus's content.

import { useEffect, useState } from 'preact/hooks'

import { removeThread } from './backend/agent'
import type { Toast } from './backend/store'
import { HOSTED, LIVE, NATIVE } from './backend/live'
import { inQuietHours, shellApi } from './backend/shell'
import { dismissToast, getState, globalStop, pauseAll, resumeAll, setLimiter, useStore } from './backend/store'
import { limiterActive } from './model/schedule.ts'
import { fmtBytes, fmtRate } from './model/units.ts'
import { commandFor, bindingOf, formatCombo } from './lib/keys'
import { getPrefs, setPrefs, usePrefs } from './lib/prefs'
import { href, navigate, useRoute } from './lib/router'
import { useWindowChrome } from './lib/shell'
import { applyPrefs, watchSystemScheme } from './lib/themes'
import { chime, openDialog, setPalette, useUi } from './lib/ui'
import { NAV } from './nav'
import { Dialog } from './ui/components'
import { Icon, Mark } from './ui/icons'
import { Palette } from './ui/Palette'
import { WindowControls } from './ui/WindowControls'
import { Agent, DetailsToggle } from './views/Agent'
import { ApprovalDetail, Approvals } from './views/Approvals'
import { Connect } from './views/Connect'
import { Connectors } from './views/Connectors'
import { AddUrl } from './views/dialogs/AddUrl'
import { SiteGrabber } from './views/dialogs/SiteGrabber'
import { Downloads, VIEWS, inView } from './views/downloads/Downloads'
import { Files } from './views/Files'
import { Uploads } from './views/Uploads'
import { Grabber } from './views/Grabber'
import { Queues } from './views/Queues'
import { SETTINGS_SECTIONS, Settings } from './views/settings'
import { Skills } from './views/Skills'

const TITLES: Record<string, string> = {
  downloads: 'Downloads',
  grabber: 'Link grabber',
  queues: 'Queues and scheduler',
  files: 'Files',
  uploads: 'Uploads',
  agent: 'Agent',
  skills: 'Skills',
  approvals: 'Approvals',
  connect: 'Connect',
  connectors: 'Connectors',
  settings: 'Settings',
  about: 'About',
}

function Page({ path, query }: { path: string[]; query: URLSearchParams }) {
  const [section, a] = path
  switch (section ?? '') {
    case '':
    case 'downloads':
      return <Downloads view={query.get('view') ?? 'all'} queue={query.get('queue')} {...(query.get('job') ? { focus: query.get('job') as string } : {})} />
    case 'grabber':
      return <Grabber />
    case 'queues':
      return <Queues {...(query.get('q') ? { queueId: query.get('q') as string } : {})} {...(query.get('tab') ? { tab: query.get('tab') as string } : {})} />
    case 'uploads':
      return <Uploads initialPath={query.get('path') ?? ''} initialName={query.get('name') ?? ''} />
    case 'files':
      return <Files {...(query.get('path') ? { initialPath: query.get('path') as string } : {})} />
    case 'agent':
      return <Agent {...(a ? { threadId: a } : {})} />
    case 'skills':
      return <Skills {...(a ? { id: a } : {})} />
    case 'approvals':
      return a ? <ApprovalDetail id={a} key={a} /> : <Approvals {...(query.get('state') ? { state: query.get('state') as string } : {})} />
    case 'connect':
      return <Connect />
    case 'connectors':
      return <Connectors {...(a ? { id: a } : {})} />
    case 'settings':
      return <Settings section={a ?? 'general'} />
    case 'about':
      return <Settings section="about" />
    default:
      return (
        <div class="empty">
          No such page. <a href={href('downloads')}>Downloads</a>
        </div>
      )
  }
}

function Toasts() {
  const toasts = useStore((s) => s.toasts)
  const prefs = usePrefs()
  const notif = useStore((s) => s.settings.notifications)
  const showDone = useStore((s) => s.settings.downloads.showCompleteDialog)
  const [seen] = useState(() => new Set<number>())
  const visible = toasts.filter((t) => {
    if (prefs.notifications === 'off' && t.topic !== 'system') return false
    if (prefs.notifications === 'important' && !t.important) return false
    if (t.topic === 'complete' && !notif.onComplete) return false
    if (t.topic === 'failure' && !notif.onFailure) return false
    if (t.topic === 'approval' && !notif.onApproval) return false
    if (t.topic === 'queue' && !notif.onQueue) return false
    return true
  })
  useEffect(() => {
    for (const t of toasts) {
      if (seen.has(t.id)) continue
      seen.add(t.id)
      if (t.sound && prefs.sounds && visible.includes(t)) chime(t.sound)
      // Away from the window, the same notices go to the system's notification area (never the agent's words or links).
      if (NATIVE && visible.includes(t) && (document.hidden || !document.hasFocus()) && !(notif.quietEnabled && inQuietHours(notif.quietFrom, notif.quietTo))) {
        void shellApi.notify(t.title, t.desc ?? '').catch(() => undefined)
      }
      const sticky = t.topic === 'complete' && showDone
      if (!sticky) setTimeout(() => dismissToast(t.id), t.kind === 'warn' || t.kind === 'bad' ? 9000 : 6000)
    }
  }, [toasts])
  if (!visible.length) return null
  return (
    <div class="toasts" aria-live="polite">
      {visible.map((t: Toast) => (
        <div class={`toast ${t.kind}`} key={t.id}>
          <span class="dot" />
          <div class="text">
            {t.to ? (
              <a class="title" href={t.to} onClick={() => dismissToast(t.id)}>
                {t.title}
              </a>
            ) : (
              <div class="title">{t.title}</div>
            )}
            {t.desc ? <div class="desc">{t.desc}</div> : null}
          </div>
          <button type="button" class="x" aria-label="Dismiss" onClick={() => dismissToast(t.id)}>
            ×
          </button>
        </div>
      ))}
    </div>
  )
}

function StatusBar() {
  const jobs = useStore((s) => s.jobs)
  const limiter = useStore((s) => s.limiter)
  const now = useStore((s) => s.now)
  const pending = useStore((s) => s.approvals.filter((a) => a.state === 'pending').length)
  const index = useStore((s) => s.index)
  const stop = useStore((s) => s.killSwitch)
  const active = jobs.filter((j) => j.state === 'downloading')
  const speed = active.reduce((n, j) => n + j.rate, 0)
  const queued = jobs.filter((j) => j.state === 'queued').length
  const on = limiterActive(limiter, now)
  return (
    <footer class="statusbar" aria-label="Status">
      <span class="sb-speed" title="Total download speed">
        <Icon name="download" /> {active.length ? fmtRate(speed) : 'idle'}
      </span>
      <span>{active.length} active</span>
      <span>{queued} queued</span>
      {pending ? (
        <a class="sb-warn" href={href('approvals')}>
          {pending} waiting for you
        </a>
      ) : null}
      {stop ? <span class="sb-bad">global stop on</span> : null}
      <span class="spacer" />
      <button type="button" class={`sb-btn ${limiter.enabled ? 'on' : ''}`} onClick={() => setLimiter({ enabled: !limiter.enabled })} title="Turn the speed limiter on or off">
        <Icon name="gauge" /> {limiter.enabled ? (on ? `limit ${fmtBytes(limiter.limitKiBps * 1024)}/s` : 'limit off-schedule') : 'no limit'}
      </button>
      <span class="hide-narrow">{index.state === 'scanning' ? `indexing ${Math.round(index.progress * 100)} %` : `index current`}</span>
      <span class="sb-pill" title={LIVE ? (NATIVE ? 'The Zeus engine on this computer: a bundled zeusd that this app set up and runs' : HOSTED ? 'A real zeusd on the pre-alpha server, in a private sandbox' : 'Your own zeusd through zeus-web-bridge') : 'This release runs on sample data with no backend'}>
        Pre-alpha release · {LIVE ? (NATIVE ? 'this computer' : HOSTED ? 'hosted zeusd' : 'your zeusd') : 'sample data'}
      </span>
    </footer>
  )
}

/** The sidebar footer: the release label and, on a live backend, whether zeusd answers. */
function ConnLink() {
  const live = useStore((s) => s.live)
  const ok = !LIVE || !!live?.connected
  const title = !LIVE ? 'This release runs on sample data with no backend' : ok ? `Connected to ${NATIVE ? 'the Zeus engine on this computer' : HOSTED ? 'the hosted zeusd' : 'your zeusd'}${live?.daemon ? ` ${live.daemon.daemon_version}` : ''}` : (live?.failure?.message ?? 'Connecting…')
  return (
    <a class={`conn ${ok ? 'ok' : 'bad'}`} href={href('settings', LIVE ? 'daemon' : 'about')} title={title}>
      <span class="dot" />
      <span class="name">Pre-alpha release</span>
    </a>
  )
}

/** A thin notice while a live backend does not answer. */
function ConnBanner() {
  const live = useStore((s) => s.live)
  if (!LIVE || !live || live.connected) return null
  return (
    <div class="notice warn conn-banner" role="status">
      {live.failure ? live.failure.message : NATIVE ? 'Starting the Zeus engine…' : HOSTED ? 'Connecting to the Zeus pre-alpha server…' : 'Connecting to your zeusd…'}{' '}
      <a href={href('settings', 'daemon')}>{NATIVE ? 'Engine' : 'Backend settings'}</a>
    </div>
  )
}

let lastOutside = '#/downloads'

function StopDialog({ onClose }: { onClose: () => void }) {
  return (
    <Dialog title="Stop everything?" onClose={onClose}>
      <p>Every transfer pauses with its saved bytes and every site permission is revoked. Releasing the stop later restores neither: you resume downloads and approve sites again.</p>
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Keep running
        </button>
        <button
          type="button"
          class="btn danger"
          onClick={() => {
            globalStop()
            onClose()
          }}
        >
          Stop everything
        </button>
      </div>
    </Dialog>
  )
}

export function App() {
  const route = useRoute()
  const prefs = usePrefs()
  const ui = useUi()
  const chrome = useWindowChrome()
  const drag = chrome === 'custom' ? { 'data-tauri-drag-region': 'deep' } : {}
  const jobs = useStore((s) => s.jobs)
  const queues = useStore((s) => s.queues)
  const threads = useStore((s) => s.threads)
  const pending = useStore((s) => s.approvals.filter((a) => a.state === 'pending').length)
  const grabberCount = useStore((s) => s.grabber.length)
  const [allThreads, setAllThreads] = useState(false)

  useEffect(() => applyPrefs(prefs), [prefs])
  useEffect(() => watchSystemScheme(() => applyPrefs(getPrefs())), [])
  useEffect(() => {
    if (!window.location.hash || window.location.hash === '#' || window.location.hash === '#/') navigate(href(getPrefs().startPage || 'downloads'))
  }, [])

  const section = route.path[0] ?? 'downloads'
  const inSettings = section === 'settings' || section === 'about'
  useEffect(() => {
    if (!inSettings) lastOutside = window.location.hash || '#/downloads'
  }, [section, route])
  // A global dialog belongs to the page it was opened on.
  useEffect(() => {
    const close = () => openDialog(null)
    window.addEventListener('hashchange', close)
    return () => window.removeEventListener('hashchange', close)
  }, [])

  // The window title carries the speed, as download managers do.
  const active = jobs.filter((j) => j.state === 'downloading')
  const speed = active.reduce((n, j) => n + j.rate, 0)
  useEffect(() => {
    document.title = prefs.titleSpeed && active.length ? `${fmtRate(speed)} · ${active.length} active · Zeus` : 'Zeus'
    if (NATIVE) void shellApi.tooltip(active.length ? `Zeus · ${fmtRate(speed)} · ${active.length} active` : 'Zeus · idle').catch(() => undefined)
  }, [prefs.titleSpeed, active.length, Math.round(speed / 65536)])

  useEffect(() => {
    const NAV_TARGETS: Record<string, string> = {
      'nav.downloads': 'downloads',
      'nav.grabber': 'grabber',
      'nav.queues': 'queues',
      'nav.files': 'files',
      'nav.agent': 'agent',
      'nav.approvals': 'approvals',
      'nav.connectors': 'connectors',
      'nav.skills': 'skills',
    }
    const onKey = (e: KeyboardEvent) => {
      const c = commandFor(e, getPrefs())
      if (!c) return
      e.preventDefault()
      switch (c.id) {
        case 'palette.toggle':
          setPalette(useUiPalette() ? null : 'all')
          break
        case 'settings.open':
          navigate(href('settings', 'general'))
          break
        case 'sidebar.toggle':
          setPrefs({ sidebarOpen: !getPrefs().sidebarOpen })
          break
        case 'theme.pick':
          setPalette('themes')
          break
        case 'appearance.cycle': {
          const order = ['system', 'light', 'dark'] as const
          setPrefs({ appearance: order[(order.indexOf(getPrefs().appearance) + 1) % order.length] as (typeof order)[number] })
          break
        }
        case 'download.add':
          openDialog({ kind: 'addUrl' })
          break
        case 'download.grabber':
          navigate(href('grabber'))
          break
        case 'download.site':
          openDialog({ kind: 'site' })
          break
        case 'download.pauseAll':
          pauseAll()
          break
        case 'download.resumeAll':
          resumeAll()
          break
        case 'limiter.toggle':
          setLimiter({ enabled: !getState().limiter.enabled })
          break
        case 'files.search':
          navigate(href('files'))
          setTimeout(() => window.dispatchEvent(new Event('zeus:files-search')), 30)
          break
        case 'agent.new':
          navigate(href('agent'))
          break
        default: {
          const to = NAV_TARGETS[c.id]
          if (to) navigate(href(to))
        }
      }
    }
    window.addEventListener('keydown', onKey)
    return () => window.removeEventListener('keydown', onKey)
  }, [])

  const view = route.query.get('view') ?? 'all'
  const queueQ = route.query.get('queue')
  const detail = route.path[1]
  const settingsId = section === 'about' ? 'about' : (route.path[1] ?? 'general')
  const settingsSection = SETTINGS_SECTIONS.find((s) => s.id === settingsId)
  const title = TITLES[section] ?? section
  const groups = [...new Set(NAV.map((n) => n.group))]
  const count = (v: string) => jobs.filter((j) => inView(j, v, null)).length
  const threadRows = allThreads ? threads : threads.slice(0, 6)
  const nextScheme = prefs.appearance === 'system' ? 'System' : prefs.appearance === 'dark' ? 'Dark' : 'Light'

  const crumbs = inSettings ? (
    <>
      <a href={href('settings', 'general')}>Settings</a>
      <span class="sep">/</span>
      <span class="here">{settingsSection?.label ?? settingsId}</span>
    </>
  ) : (
    <>
      <a href={href('downloads')}>zeus</a>
      <span class="sep">/</span>
      {detail || (section === 'downloads' && (view !== 'all' || queueQ)) ? (
        <>
          <a href={href(section)}>{title}</a>
          <span class="sep">/</span>
          <span class="here">
            {section === 'agent' ? (threads.find((t) => t.id === detail)?.title ?? 'Thread') : section === 'downloads' ? (queueQ ? (queues.find((q) => q.id === queueQ)?.name ?? queueQ) : (VIEWS.find((v) => v.id === view)?.label ?? view)) : <span class="mono">{detail}</span>}
          </span>
        </>
      ) : (
        <span class="here">{title}</span>
      )}
    </>
  )

  const fullBleed = section === 'downloads' || section === '' || section === 'files'
  return (
    <>
      <a class="skip" href="#main">
        Skip to content
      </a>
      <div class={`shell ${prefs.sidebarOpen ? '' : 'side-closed'}${chrome ? ` chrome-${chrome}` : ''}${section === 'agent' ? ' is-chat' : ''}`}>
        <aside class="side" aria-label={inSettings ? 'Settings sections' : 'Sections'}>
          <div class="brand" {...drag}>
            <button type="button" class="icon-btn brand-toggle" onClick={() => setPrefs({ sidebarOpen: false })} aria-label="Hide the sidebar" title={`Hide the sidebar (${formatCombo(bindingOf(prefs, 'sidebar.toggle')).join('+')})`}>
              <Icon name="panel" />
            </button>
            <a class="brand-link" href={href('downloads')}>
              <Mark />
              <span class="brand-name">Zeus</span>
            </a>
            <span class="spacer" />
          </div>
          {!inSettings ? (
            <button type="button" class="newthread" onClick={() => openDialog({ kind: 'addUrl' })} title={`Add URL (${formatCombo(bindingOf(prefs, 'download.add')).join('+')})`}>
              <Icon name="plus" />
              Add URL
              <span class="spacer" />
              {formatCombo(bindingOf(prefs, 'download.add')).map((k) => (
                <kbd>{k}</kbd>
              ))}
            </button>
          ) : null}
          <button type="button" class="side-search" onClick={() => setPalette('all')}>
            <Icon name="search" />
            Search
            <span class="spacer" />
            {formatCombo(bindingOf(prefs, 'palette.toggle')).map((k) => (
              <kbd>{k}</kbd>
            ))}
          </button>
          {inSettings ? (
            <nav class="nav settings-nav">
              {SETTINGS_SECTIONS.map((s) => (
                <a href={href('settings', s.id)} aria-current={settingsId === s.id ? 'page' : undefined}>
                  <Icon name={s.icon} />
                  <span class="name">{s.label}</span>
                </a>
              ))}
            </nav>
          ) : (
            <nav class="nav">
              {groups.map((g) => (
                <div class="nav-group" key={g}>
                  <div class="nav-head">{g}</div>
                  {NAV.filter((n) => n.group === g).map((n) => (
                    <a href={href(n.id)} aria-current={section === n.id && !(n.id === 'downloads' && (view !== 'all' || queueQ)) ? 'page' : undefined}>
                      <Icon name={n.icon} />
                      <span class="name">{n.label}</span>
                      {n.id === 'approvals' && pending ? <span class="count" aria-label={`${pending} pending`}>{pending}</span> : null}
                      {n.id === 'grabber' && grabberCount ? <span class="trail">{grabberCount}</span> : null}
                      {n.id === 'downloads' ? <span class="trail">{active.length ? fmtRate(speed) : ''}</span> : null}
                    </a>
                  ))}
                </div>
              ))}
              {prefs.sidebarCategories ? (
                <>
                  <div class="nav-group" aria-label="Categories">
                    <div class="nav-head">Categories</div>
                    {VIEWS.map((v) => (
                      <a href={href('downloads', { view: v.id })} aria-current={section === 'downloads' && view === v.id && !queueQ && v.id !== 'all' ? 'page' : undefined}>
                        <Icon name={v.id === 'all' ? 'inbox' : v.id === 'unfinished' ? 'download' : v.id === 'finished' ? 'check' : v.id === 'grabber' ? 'globe' : v.id === 'compressed' ? 'archive' : v.id === 'documents' ? 'doc' : v.id === 'music' ? 'music' : v.id === 'programs' ? 'program' : v.id === 'video' ? 'video' : 'picture'} />
                        <span class="name">{v.label}</span>
                        <span class="trail">{count(v.id)}</span>
                      </a>
                    ))}
                  </div>
                  <div class="nav-group" aria-label="Queues">
                    <div class="nav-head">Queues</div>
                    {queues.map((q) => (
                      <a href={href('downloads', { queue: q.id })} aria-current={section === 'downloads' && queueQ === q.id ? 'page' : undefined}>
                        <span class={`dot ${q.running ? 'run' : ''}`} aria-hidden="true" />
                        <span class="name">{q.name}</span>
                        <span class="trail">{q.running ? 'running' : jobs.filter((j) => j.queue === q.id && j.state !== 'completed' && j.state !== 'canceled').length}</span>
                      </a>
                    ))}
                  </div>
                </>
              ) : null}
              {threads.length ? (
                <div class="nav-group" aria-label="Agent threads">
                  <div class="nav-head">Threads</div>
                  {threadRows.map((t) => (
                    <a class="threadrow" href={href('agent', t.id)} aria-current={section === 'agent' && detail === t.id ? 'page' : undefined} title={t.title}>
                      <span class={`dot ${t.state === 'running' ? 'run' : ''}`} aria-hidden="true" />
                      <span class="name">{t.title}</span>
                      <span class="trail">{agoShort(t.updated)}</span>
                      <button
                        type="button"
                        class="rowx"
                        aria-label={`Delete thread ${t.title}`}
                        title="Delete this thread"
                        onClick={(e) => {
                          e.preventDefault()
                          e.stopPropagation()
                          removeThread(t.id)
                          if (detail === t.id) navigate(href('agent'))
                        }}
                      >
                        <Icon name="trash" />
                      </button>
                    </a>
                  ))}
                  {threads.length > 6 ? (
                    <button type="button" class="showmore" onClick={() => setAllThreads((v) => !v)}>
                      <Icon name={allThreads ? 'up' : 'plus'} /> {allThreads ? 'Show fewer' : `Show ${threads.length - 6} more`}
                    </button>
                  ) : null}
                </div>
              ) : null}
            </nav>
          )}
          <div class="side-foot">
            {inSettings ? (
              <a class="backlink" href={lastOutside}>
                <Icon name="arrowleft" /> Back
              </a>
            ) : (
              <>
                <a class="icon-btn" href={href('settings', 'general')} aria-label="Settings" title={`Settings (${formatCombo(bindingOf(prefs, 'settings.open')).join('+')})`}>
                  <Icon name="gear" />
                </a>
                <a class="icon-btn" href={href('settings', 'about')} aria-label="About" title="About this release">
                  <Icon name="info" />
                </a>
                <button type="button" class="icon-btn" onClick={() => setPalette('themes')} aria-label={`Appearance: ${nextScheme}. Change theme.`} title={`Appearance: ${nextScheme}`}>
                  <Icon name={prefs.appearance === 'dark' ? 'moon' : prefs.appearance === 'light' ? 'sun' : 'contrast'} />
                </button>
              </>
            )}
            <span class="spacer" />
            <ConnLink />
          </div>
        </aside>
        <header class="top" {...drag}>
          <button type="button" class="icon-btn top-toggle" onClick={() => setPrefs({ sidebarOpen: true })} aria-label="Show the sidebar">
            <Icon name="panel" />
          </button>
          <nav class="crumbs" aria-label="Breadcrumb">
            {crumbs}
          </nav>
          <span class="spacer" />
          {section === 'agent' && detail ? <DetailsToggle /> : null}
          <button type="button" class="icon-btn hide-narrow" aria-label="Search" title="Search (Ctrl+K)" onClick={() => setPalette('all')}>
            <Icon name="search" />
          </button>
          {chrome === 'custom' ? <WindowControls /> : null}
        </header>
        <main class={`main ${section === 'agent' ? 'chat' : ''} ${fullBleed ? 'bleed' : ''}`} id="main">
          <ConnBanner />
          <Page path={route.path} query={route.query} />
        </main>
        {prefs.statusBar ? <StatusBar /> : null}
      </div>
      <nav class="mobile-nav" aria-label="Sections">
        {[...NAV.filter((n) => ['downloads', 'grabber', 'files', 'agent', 'approvals'].includes(n.id)), { id: 'settings', label: 'Settings', icon: 'gear' as const, group: '' }].map((n) => (
          <a href={href(n.id)} aria-current={section === n.id ? 'page' : undefined}>
            <Icon name={n.icon} />
            {n.id === 'grabber' ? 'Grabber' : n.label}
          </a>
        ))}
      </nav>
      {ui.palette ? <Palette mode={ui.palette} onClose={() => setPalette(null)} go={(to) => navigate(to)} /> : null}
      {ui.dialog?.kind === 'addUrl' ? <AddUrl {...(ui.dialog.url ? { initial: ui.dialog.url } : {})} onClose={() => openDialog(null)} /> : null}
      {ui.dialog?.kind === 'site' ? <SiteGrabber onClose={() => openDialog(null)} /> : null}
      {ui.dialog?.kind === 'stop' ? <StopDialog onClose={() => openDialog(null)} /> : null}
      <Toasts />
    </>
  )
}

function useUiPalette(): boolean {
  return document.querySelector('.palette') !== null
}

function agoShort(t: number): string {
  const s = Math.round((Date.now() - t) / 1000)
  if (s < 60) return 'now'
  if (s < 3600) return `${Math.floor(s / 60)}m`
  if (s < 86400) return `${Math.floor(s / 3600)}h`
  return `${Math.floor(s / 86400)}d`
}
