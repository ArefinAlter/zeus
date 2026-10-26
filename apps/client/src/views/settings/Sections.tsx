// The settings sections that are Zeus's own: the options of a download manager (file types, save to, downloads,
// connection, site logins, browser integration, scheduler, notifications), the index options of a file search tool,
// the agent, storage and cleanup, security and the daemon link. Where zeusd has the same setting the row names its key;
// rows zeusd lacks are marked planned. In this release every change applies to the sample data of this session.

import { useState } from 'preact/hooks'

import { NATIVE, HOSTED, LIVE, applyEngineSettings } from '../../backend/live'
import { defaultDir, releaseStop, rescan, toast, updateSettings, useStore } from '../../backend/store'
import { ACCESS_INFO, ACCESS_ORDER, HUMAN_ONLY, NEVER, TOOLS } from '../../model/agentTools.ts'
import { CATEGORIES, defaultMap, overlaps, parseExtList } from '../../model/categories.ts'
import { folderProblem, insideRoot } from '../../model/grab.ts'
import { SEP, isAbsolute, joinPath } from '../../lib/paths.ts'
import type { AgentAccess, CategoryId } from '../../model/types.ts'
import { fmtBytes, fmtWhen } from '../../model/units.ts'
import { setPrefs, usePrefs } from '../../lib/prefs'
import type { Notifications } from '../../lib/prefs'
import { href } from '../../lib/router'
import { openDialog } from '../../lib/ui'
import { Chip, CodeCopy, Notice, NumberInput, PageHead, Provenance, Select, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { BrowserIntegrationSettings } from '../integration/BrowserSettings'
import { NativeIndexSettings } from './IndexNative'

const NOTE = LIVE ? (HOSTED ? 'Changes are kept for your sandbox by the pre-alpha service.' : 'Changes are kept on this device.') : 'Changes apply to this session\'s sample data.'

export function GeneralSettings() {
  const g = useStore((s) => s.settings.general)
  const p = usePrefs()
  const set = (patch: Partial<typeof g>) => updateSettings('general', patch)
  return (
    <>
      <PageHead title="General" sub={NOTE} />
      <SettingsGroup title="Start">
        <SettingRow title="Start page" desc="The page the app opens on.">
          <Select label="Start page" value={p.startPage} onChange={(v) => setPrefs({ startPage: v })} options={[['downloads', 'Downloads'], ['agent', 'Agent'], ['files', 'Files'], ['grabber', 'Link grabber'], ['queues', 'Queues']]} />
        </SettingRow>
        <SettingRow title="Launch at sign-in" desc="Start Zeus when you sign in to your computer." planned>
          <Switch label="Launch at sign-in" checked={g.launchAtLogin} onChange={(v) => set({ launchAtLogin: v })} />
        </SettingRow>
        <SettingRow title="Start minimized" planned>
          <Switch label="Start minimized" checked={g.startMinimized} onChange={(v) => set({ startMinimized: v })} />
        </SettingRow>
        <SettingRow title="Icon in the notification area" desc="Shows speed and lets you pause everything from the tray." planned>
          <Switch label="Icon in the notification area" checked={g.showTray} onChange={(v) => set({ showTray: v })} />
        </SettingRow>
        <SettingRow title="Closing the window keeps downloads running" desc="zeusd runs separately from this window, so closing it never stops a transfer.">
          <Switch label="Closing keeps downloads running" checked={g.closeToTray} onChange={(v) => set({ closeToTray: v })} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Behaviour">
        <SettingRow title="Ask before removing downloads" desc="Removing an unfinished download deletes its partial data.">
          <Switch label="Ask before removing" checked={p.confirmRemove} onChange={(v) => setPrefs({ confirmRemove: v })} />
        </SettingRow>
        <SettingRow title="Speed in the window title">
          <Switch label="Speed in the window title" checked={p.titleSpeed} onChange={(v) => setPrefs({ titleSpeed: v })} />
        </SettingRow>
        <SettingRow title="Time format">
          <Select label="Time format" value={g.timeFormat} onChange={(v) => set({ timeFormat: v })} options={[['24h', '24-hour'], ['12h', '12-hour']]} />
        </SettingRow>
        <SettingRow title="Language" planned>
          <Select label="Language" value={g.language} onChange={(v) => set({ language: v })} options={[['English', 'English']]} />
        </SettingRow>
        <SettingRow title="Check for updates" desc="No telemetry. A check would only ask for the latest version number." planned>
          <Select label="Check for updates" value={g.checkUpdates} onChange={(v) => set({ checkUpdates: v })} options={[['never', 'Never'], ['weekly', 'Weekly']]} />
        </SettingRow>
      </SettingsGroup>
    </>
  )
}

export function FileTypesSettings() {
  const map = useStore((s) => s.settings.categories)
  const saveTo = useStore((s) => s.settings.saveTo)
  const [text, setText] = useState<Record<string, string>>(() => Object.fromEntries(Object.entries(map).map(([k, v]) => [k, v.join(' ')])))
  const clash = overlaps(map)
  const folderOf = (id: CategoryId) => (NATIVE ? defaultDir(id) : `…${SEP()}${CATEGORIES.find((c) => c.id === id)?.folder ?? ''}`)
  return (
    <>
      <PageHead title="File types and categories" sub="Which extensions belong to which category; a category decides the folder a download saves to." />
      <SettingsGroup title="Categories" note="An extension can belong to one category only. Unknown types go to General. The set of categories is fixed in this release; the extensions in each are yours to change.">
        {CATEGORIES.filter((c) => c.id !== 'general').map((c) => (
          <SettingRow title={c.label} desc={`Saves to ${folderOf(c.id)}`} stack>
            <input
              class="mono"
              aria-label={`${c.label} extensions`}
              value={text[c.id] ?? ''}
              onInput={(e) => setText({ ...text, [c.id]: (e.target as HTMLInputElement).value })}
              onBlur={() => updateSettings('categories', { ...map, [c.id]: parseExtList(text[c.id] ?? '') })}
            />
          </SettingRow>
        ))}
        <SettingRow title="Back to the defaults" desc="Restores the extension lists Zeus ships with. Folders you chose in Save to stay.">
          <button
            type="button"
            class="btn small"
            onClick={() => {
              const d = defaultMap()
              updateSettings('categories', d)
              setText(Object.fromEntries(Object.entries(d).map(([k, v]) => [k, v.join(' ')])))
            }}
          >
            Restore defaults
          </button>
        </SettingRow>
      </SettingsGroup>
      {clash.length ? <Notice kind="warn">{clash.map((c) => `.${c.ext} is in ${c.categories.join(' and ')}`).join('; ')}. Remove it from one of them.</Notice> : null}
      {NATIVE && !saveTo.perCategory ? <Notice>"A folder per category" is off in Save to, so every category saves to the default folder unless it has a folder of its own there.</Notice> : null}
      <Provenance>zeusd saves into its download folders without categories; categories and these rules belong to this app and decide the folder each download is sent to.</Provenance>
    </>
  )
}

export function SaveToSettings() {
  const s = useStore((st) => st.settings.saveTo)
  const dirty = useStore((st) => !!st.live?.engineDirty)
  const set = (patch: Partial<typeof s>) => updateSettings('saveTo', patch)
  const [root, setRoot] = useState('')
  const [busy, setBusy] = useState(false)
  const ci = SEP() === '\\' || !NATIVE
  const rootProblem = root.trim() && NATIVE ? (!isAbsolute(root.trim()) ? 'Use a full folder path.' : s.writeRoots.some((r) => insideRoot(r, root.trim(), ci) && insideRoot(root.trim(), r, ci)) ? 'That folder is already one of them.' : null) : null
  const dirs = s.categoryDirs ?? {}
  const last = s.lastDirs ?? {}
  const setCategoryDir = (id: string, value: string) => {
    const next = { ...dirs }
    if (value.trim()) next[id] = value.trim()
    else delete next[id]
    set({ categoryDirs: next })
  }
  const changeDefault = (value: string) => {
    const v = value.trim()
    if (!NATIVE) return set({ root: v })
    if (!v) return
    // The default folder is the first folder zeusd may write to.
    set({ root: v, writeRoots: [v, ...s.writeRoots.slice(1).filter((r) => r !== v)] })
  }
  return (
    <>
      <PageHead title="Save to" sub={NOTE} />
      {NATIVE && dirty ? (
        <Notice kind="warn">
          The download folders changed. zeusd reads them when it starts, so they take effect after the engine restarts; downloads in progress resume.{' '}
          <button
            type="button"
            class="btn small primary"
            disabled={busy}
            onClick={() => {
              setBusy(true)
              applyEngineSettings().then(
                () => toast({ kind: 'ok', title: 'Download folders applied', desc: 'zeusd restarted with the new folders.', important: false, topic: 'system' }),
                (e: unknown) => toast({ kind: 'bad', title: 'Not applied', desc: e instanceof Error ? e.message : 'The folders were refused.', important: true, topic: 'system' }),
              ).finally(() => setBusy(false))
            }}
          >
            Apply and restart the engine
          </button>
        </Notice>
      ) : null}
      <SettingsGroup title="Folders">
        <SettingRow title="Default folder" zeusd="write_roots[0]">
          <input class="mono wideinput" aria-label="Default folder" value={s.root} onChange={(e) => changeDefault((e.target as HTMLInputElement).value)} />
        </SettingRow>
        <SettingRow title="A folder per category" desc="Documents go to …\Documents, videos to …\Video, and so on, under the default folder. A folder of its own for a category (below) wins.">
          <Switch label="A folder per category" checked={s.perCategory} onChange={(v) => set({ perCategory: v })} />
        </SettingRow>
        <SettingRow title="Partial files" desc="zeusd keeps unfinished data beside the final file as <name>.zpart and renames it atomically when the file is verified; its records are in the .zeus folder of each download folder.">
          <Chip tone="info">beside each file</Chip>
        </SettingRow>
        <SettingRow title="Ask where to save each download" desc="Add URL asks you to confirm the folder before it starts. Links from the link grabber and the site grabber show their folder in the review.">
          <Switch label="Ask each time" checked={s.askEachTime} onChange={(v) => set({ askEachTime: v })} />
        </SettingRow>
        <SettingRow title="Remember the last folder used for a category" desc="When you save a type of file somewhere else once, the next one of that type goes there too.">
          <Switch label="Remember the last folder" checked={s.rememberLast} onChange={(v) => set({ rememberLast: v, ...(v ? {} : { lastDirs: {} }) })} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="A folder for each category" note="Leave empty for the default subfolder. A folder must be inside a download folder; one outside is ignored.">
        {CATEGORIES.filter((c) => c.id !== 'general').map((c) => {
          const v = dirs[c.id] ?? ''
          const problem = v && NATIVE ? folderProblem(v, s.writeRoots, ci) : null
          return (
            <SettingRow title={c.label} desc={problem ?? (last[c.id] && s.rememberLast ? `Last used: ${last[c.id]}` : undefined)} stack>
              <input
                class={`mono ${problem ? 'bad' : ''}`}
                aria-label={`Folder for ${c.label}`}
                placeholder={NATIVE ? defaultDirFor(c.id, s, ci) : `…${SEP()}${c.folder}`}
                value={v}
                onChange={(e) => setCategoryDir(c.id, (e.target as HTMLInputElement).value)}
              />
              {last[c.id] && s.rememberLast ? (
                <button type="button" class="btn small ghost" onClick={() => set({ lastDirs: Object.fromEntries(Object.entries(last).filter(([k]) => k !== c.id)) })}>
                  Forget the last folder
                </button>
              ) : null}
            </SettingRow>
          )
        })}
      </SettingsGroup>
      <SettingsGroup title="Folders Zeus may write to" note="Zeus writes only inside these folders, whatever a page, a file name or an agent asks. The folders are written to zeusd's configuration, which this app does for you when you apply them; an agent or another program cannot change them.">
        {s.writeRoots.map((r, i) => (
          <SettingRow title={<span class="mono">{r}</span>} desc={i === 0 ? 'The default folder' : undefined}>
            <button type="button" class="btn small ghost" disabled={s.writeRoots.length < 2 || i === 0} onClick={() => set({ writeRoots: s.writeRoots.filter((x) => x !== r) })}>
              Remove
            </button>
          </SettingRow>
        ))}
        <div class="row">
          <div class="text">
            <input class={`mono ${rootProblem ? 'bad' : ''}`} aria-label="New folder" placeholder={SEP() === '/' ? '/mnt/media/Zeus' : 'F:\\Media\\Zeus'} value={root} onInput={(e) => setRoot((e.target as HTMLInputElement).value)} />
            {rootProblem ? <p class="field-err">{rootProblem}</p> : null}
          </div>
          <div class="control">
            <button type="button" class="btn small" disabled={!/^[A-Za-z]:\\|^\\\\|^\//.test(root.trim()) || !!rootProblem} onClick={() => (set({ writeRoots: [...s.writeRoots, root.trim()] }), setRoot(''))}>
              <Icon name="plus" /> Add folder
            </button>
          </div>
        </div>
      </SettingsGroup>
    </>
  )
}

function defaultDirFor(id: CategoryId, s: { root: string; perCategory: boolean }, _ci: boolean): string {
  const sub = CATEGORIES.find((c) => c.id === id)?.folder
  return s.perCategory && sub ? joinPath(s.root, sub) : s.root
}

export function DownloadsSettings() {
  const d = useStore((s) => s.settings.downloads)
  const set = (patch: Partial<typeof d>) => updateSettings('downloads', patch)
  return (
    <>
      <PageHead title="Downloads" sub={NOTE} />
      <SettingsGroup title="Connections">
        <SettingRow title="Files at the same time" desc="Across all queues." zeusd="scheduler.concurrent_jobs">
          <NumberInput label="Files at the same time" value={d.concurrentJobs} min={1} max={64} onChange={(v) => set({ concurrentJobs: v })} />
        </SettingRow>
        <SettingRow title="Connections per file" desc="An upper bound; the engine may use fewer when a server prefers it." zeusd="scheduler.connections_per_job">
          <NumberInput label="Connections per file" value={d.connectionsPerJob} min={1} max={32} onChange={(v) => set({ connectionsPerJob: v })} />
        </SettingRow>
        <SettingRow title="Connections per host" desc="Across all files from one host." zeusd="scheduler.connections_per_host">
          <NumberInput label="Connections per host" value={d.connectionsPerHost} min={1} max={64} onChange={(v) => set({ connectionsPerHost: v })} />
        </SettingRow>
        <SettingRow title="Segment size" desc="zeusd refuses changes to this key: its engine requests ranges by commit unit, not by this size." zeusd="scheduler.segment_size_mib">
          <NumberInput label="Segment size" value={d.segmentMiB} min={8} max={32} onChange={(v) => set({ segmentMiB: v })} unit="MiB" />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Retries and timeouts">
        <SettingRow title="Retries" desc="Retryable failures only (a timeout, a busy server). A failure that needs you stops at once.">
          <NumberInput label="Retries" value={d.retries} min={0} max={50} onChange={(v) => set({ retries: v })} />
        </SettingRow>
        <SettingRow title="First retry after" desc="Grows after each attempt. A server's Retry-After always wins.">
          <NumberInput label="First retry after" value={d.retryDelayS} min={1} max={3600} onChange={(v) => set({ retryDelayS: v })} unit="s" />
        </SettingRow>
        <SettingRow title="Connect timeout" zeusd="timeouts.connect_s">
          <NumberInput label="Connect timeout" value={d.connectS} min={1} max={120} onChange={(v) => set({ connectS: v })} unit="s" />
        </SettingRow>
        <SettingRow title="Read timeout" zeusd="timeouts.read_s">
          <NumberInput label="Read timeout" value={d.readS} min={5} max={300} onChange={(v) => set({ readS: v })} unit="s" />
        </SettingRow>
        <SettingRow title="Probe deadline" zeusd="timeouts.probe_s">
          <NumberInput label="Probe deadline" value={d.probeS} min={5} max={300} onChange={(v) => set({ probeS: v })} unit="s" />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Resume, duplicates and checks">
        <SettingRow title="Resuming" desc="Zeus resumes only when the server reports the same file (strong ETag or Last-Modified); otherwise it starts over rather than join two versions.">
          <Select label="Resuming" value={d.resume} onChange={(v) => set({ resume: v })} options={[['verified_only', 'Only when the file is the same'], ['ask', 'Ask me when it changed'], ['restart', 'Always start over']]} />
        </SettingRow>
        <SettingRow title="When the name exists" desc="Replace moves the old file to the app trash first." zeusd="jobs: overwrite">
          <Select label="When the name exists" value={d.duplicates} onChange={(v) => set({ duplicates: v })} options={[['rename', 'Add (1), (2)…'], ['fail', 'Stop with an error'], ['replace', 'Replace (old one to trash)']]} />
        </SettingRow>
        <SettingRow title="Checksums" desc="Every finished file is hashed with SHA-256 and BLAKE3 and gets a provenance receipt; this is not optional." fixed>
          <Chip tone="ok">SHA-256 and BLAKE3</Chip>
        </SettingRow>
        <SettingRow title="Start downloads at once" desc="Off: the link grabber and the site grabber preselect the default queue instead of Start now. Add URL always has both buttons.">
          <Switch label="Start at once" checked={d.startImmediately} onChange={(v) => set({ startImmediately: v })} />
        </SettingRow>
        <SettingRow title="Show a dialog when a download finishes" desc="A notification states what finished; it never asks for a decision.">
          <Switch label="Show a dialog when finished" checked={d.showCompleteDialog} onChange={(v) => set({ showCompleteDialog: v })} />
        </SettingRow>
        <SettingRow title="Keep records of removed downloads" zeusd="tombstone_days">
          <NumberInput label="Keep records" value={d.tombstoneDays} min={1} max={365} onChange={(v) => set({ tombstoneDays: v })} unit="days" />
        </SettingRow>
      </SettingsGroup>
    </>
  )
}

export function ConnectionSettings() {
  const c = useStore((s) => s.settings.connection)
  const set = (patch: Partial<typeof c>) => updateSettings('connection', patch)
  return (
    <>
      <PageHead title="Connection and proxy" sub={NOTE} />
      <SettingsGroup title="Network">
        <SettingRow title="Connection" desc="On a metered connection Zeus can hold large downloads." planned>
          <Select label="Connection" value={c.type} onChange={(v) => set({ type: v })} options={[['auto', 'Detect'], ['lan', 'Unmetered'], ['cellular', 'Metered']]} />
        </SettingRow>
        <SettingRow title="Pause on a metered connection" planned>
          <Switch label="Pause on metered" checked={c.meteredPause} onChange={(v) => set({ meteredPause: v })} />
        </SettingRow>
        <SettingRow title="Use IPv6">
          <Switch label="IPv6" checked={c.ipv6} onChange={(v) => set({ ipv6: v })} />
        </SettingRow>
        <SettingRow title="Identify as" desc="Zeus sends its own user agent. It does not pretend to be a browser to get past a check.">
          <Select label="Identify as" value={c.userAgent} onChange={(v) => set({ userAgent: v })} options={[['zeus', 'Zeus'], ['browser', 'Zeus, with the browser family noted']]} />
        </SettingRow>
        <SettingRow title="Invalid certificates" desc="A download over https with an invalid certificate always fails." fixed>
          <Chip tone="info">always refused</Chip>
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Proxy" note="A proxy password is entered at a terminal into the keyring, never here.">
        <SettingRow title="Proxy" planned>
          <Select label="Proxy" value={c.proxy} onChange={(v) => set({ proxy: v })} options={[['system', 'Use the system proxy'], ['none', 'No proxy'], ['http', 'HTTP proxy'], ['socks5', 'SOCKS5 proxy']]} />
        </SettingRow>
        {c.proxy === 'http' || c.proxy === 'socks5' ? (
          <>
            <SettingRow title="Host and port">
              <input class="mono" aria-label="Proxy host" placeholder="proxy.example.net" value={c.proxyHost} onChange={(e) => set({ proxyHost: (e.target as HTMLInputElement).value })} />
              <NumberInput label="Port" value={c.proxyPort} min={1} max={65535} onChange={(v) => set({ proxyPort: v })} />
            </SettingRow>
            <SettingRow title="No proxy for" stack>
              <input class="mono" aria-label="No proxy for" value={c.proxyBypass} onChange={(e) => set({ proxyBypass: (e.target as HTMLInputElement).value })} />
            </SettingRow>
          </>
        ) : null}
      </SettingsGroup>
    </>
  )
}

export function SiteLoginsSettings() {
  const logins = useStore((s) => s.settings.siteLogins)
  const now = useStore((s) => s.now)
  return (
    <>
      <PageHead title="Site logins" sub="Sites where you let Zeus use your browser session, and sites you blocked. Names and scopes only." />
      <Notice>
        <strong>Zeus never stores a password or reads your browser's cookies.</strong> When a page or a file needs your login, the Zeus extension supplies the session from your browser, in memory only and only for a site you approved: once, for 24 hours, for one plan, or never.
      </Notice>
      <SettingsGroup title="Permissions">
        {logins.length ? (
          logins.map((l) => (
            <SettingRow title={<span class="mono">{l.origin}</span>} desc={`${l.label} · ${l.scope === 'never' ? 'blocked' : l.scope === 'site' ? `24 hours${l.expires ? `, until ${fmtWhen(l.expires, now)}` : ''}` : l.scope === 'once' ? 'once, for the next file' : 'one plan'} · decided via ${l.via === 'extension' ? 'the extension popup' : 'a terminal'}`}>
              <Chip tone={l.scope === 'never' ? 'bad' : 'ok'}>{l.scope === 'never' ? 'never' : l.scope}</Chip>
              <button type="button" class="btn small ghost" onClick={() => updateSettings('siteLogins', logins.filter((x) => x !== l))}>
                {l.scope === 'never' ? 'Unblock' : 'Revoke'}
              </button>
            </SettingRow>
          ))
        ) : (
          <SettingRow title="No site permissions" desc="Zeus asks you the first time a download needs a login." />
        )}
      </SettingsGroup>
      <SettingsGroup title="Rules" note="These follow Zeus ADR 0004 and spec 07; no setting changes them.">
        <SettingRow title="Cookies stay in the browser" desc="Session material reaches zeusd through the extension and native messaging, is held for at most 15 minutes in memory, and is never written to disk or a log." fixed />
        <SettingRow title="An agent cannot grant a login" desc="An agent may ask with zeus_session_request; only you approve, in the extension popup, a terminal or this app." fixed />
        <SettingRow title="A 24-hour permission is the longest by default" desc="A site permission lasts at most 7 days even when configured; never lasts until you remove it." fixed />
      </SettingsGroup>
    </>
  )
}

export function BrowserSettings() {
  if (NATIVE) return <BrowserIntegrationSettings />
  const b = useStore((s) => s.settings.browser)
  const set = (patch: Partial<typeof b>) => updateSettings('browser', patch)
  return (
    <>
      <PageHead title="Browser integration" sub={NOTE} />
      <SettingsGroup title="The Zeus extension" note="The extension hands over downloads the browser gave up on or that you forward, and supplies site sessions you approve. It is the only path between your browser and Zeus.">
        <SettingRow title="Status" desc={LIVE ? (HOSTED ? 'The hosted pre-alpha uses no browser extension and no browser session.' : 'Install the Zeus extension and zeus-native-host to connect your browser to this zeusd.') : b.extension === 'connected' ? 'Connected through native messaging (sample state).' : 'Not installed.'}>
          <Chip tone={!LIVE && b.extension === 'connected' ? 'ok' : 'warn'}>{!LIVE && b.extension === 'connected' ? 'connected' : 'not connected'}</Chip>
        </SettingRow>
        {b.browsers.map((br, i) => (
          <SettingRow title={br.name} desc={`Profile: ${br.profile}. Permissions are per profile; a site approved in one profile is not used from another.`}>
            <Switch label={`${br.name} enabled`} checked={br.enabled} onChange={(v) => set({ browsers: b.browsers.map((x, j) => (j === i ? { ...x, enabled: v } : x)) })} />
          </SettingRow>
        ))}
      </SettingsGroup>
      <SettingsGroup title="Taking over downloads">
        <SettingRow title="Offer failed browser downloads to Zeus" desc="A download that failed in the browser can continue in Zeus from where it stopped, after you approve the handoff.">
          <Switch label="Offer failed downloads" checked={b.takeOverFailed} onChange={(v) => set({ takeOverFailed: v })} />
        </SettingRow>
        <SettingRow title="File types to offer" desc="Extensions, space separated." stack>
          <input class="mono" aria-label="File types to offer" value={b.monitoredTypes} onChange={(e) => set({ monitoredTypes: (e.target as HTMLInputElement).value })} />
        </SettingRow>
        <SettingRow title="Only files larger than">
          <NumberInput label="Minimum size" value={b.minSizeKiB} min={0} max={1_048_576} onChange={(v) => set({ minSizeKiB: v })} unit="KiB" />
        </SettingRow>
        <SettingRow title="Hold a key to let the browser keep a download" planned>
          <Select label="Bypass key" value={b.hotkeyBypass} onChange={(v) => set({ hotkeyBypass: v })} options={[['alt', 'Alt'], ['ctrl', 'Ctrl'], ['none', 'None']]} />
        </SettingRow>
        <SettingRow title="Download with Zeus in the browser's link menu" planned>
          <Switch label="Link menu" checked={b.contextMenu} onChange={(v) => set({ contextMenu: v })} />
        </SettingRow>
      </SettingsGroup>
    </>
  )
}

export function SchedulerSettings() {
  const s = useStore((st) => st.settings.scheduler)
  const queues = useStore((st) => st.queues)
  const set = (patch: Partial<typeof s>) => updateSettings('scheduler', patch)
  return (
    <>
      <PageHead title="Scheduler defaults" sub={NOTE} />
      <SettingsGroup title="Defaults" note="Each queue has its own times and limits on the Queues page.">
        <SettingRow title="Queue for Download later">
          <Select label="Default queue" value={s.defaultQueue} onChange={(v) => set({ defaultQueue: v })} options={queues.filter((q) => q.kind === 'download').map((q) => [q.id, q.name] as [string, string])} />
        </SettingRow>
        <SettingRow title="Times are in">
          <Select label="Time zone" value={s.timezone} onChange={(v) => set({ timezone: v })} options={[['local', 'Local time'], ['utc', 'UTC']]} />
        </SettingRow>
        <SettingRow title="A start missed while the computer was off" desc="Whether a queue starts late when the computer wakes after its start time.">
          <Select label="Missed start" value={s.missedWindow} onChange={(v) => set({ missedWindow: v })} options={[['skip', 'Skip it, wait for the next one'], ['run_once', 'Start once on waking']]} />
        </SettingRow>
        <SettingRow title="At a stop time">
          <Select label="At a stop time" value={s.stopBehaviour} onChange={(v) => set({ stopBehaviour: v })} options={[['pause', 'Pause at once, keep saved bytes'], ['finish_current', 'Let active files finish']]} />
        </SettingRow>
      </SettingsGroup>
      <p>
        <a href={href('queues')}>Open the queues and scheduler</a>
      </p>
      <Provenance>{LIVE ? 'zeusd does not schedule queues yet. These defaults apply to the queues this client runs.' : "zeusd does not schedule queues yet. These defaults apply to this release's queues."}</Provenance>
    </>
  )
}

export function LimiterSettings() {
  const l = useStore((s) => s.limiter)
  return (
    <>
      <PageHead title="Speed limiter" />
      <SettingsGroup>
        <SettingRow title="Speed limiter" desc={l.enabled ? `${fmtBytes(l.limitKiBps * 1024)}/s${l.scheduleEnabled ? `, ${l.from} to ${l.to}` : ''}` : 'Off'}>
          <a class="btn small" href={href('queues', { tab: 'limiter' })}>
            Change
          </a>
        </SettingRow>
      </SettingsGroup>
    </>
  )
}

export function NotificationSettings() {
  const n = useStore((s) => s.settings.notifications)
  const p = usePrefs()
  const set = (patch: Partial<typeof n>) => updateSettings('notifications', patch)
  return (
    <>
      <PageHead title="Notifications and sounds" sub="A notification states a fact and the next step. It never asks for a decision; decisions need a trust surface." />
      <SettingsGroup title="Show">
        <SettingRow title="Notifications">
          <Select<Notifications> label="Notifications" value={p.notifications} onChange={(v) => setPrefs({ notifications: v })} options={[['all', 'All'], ['important', 'Important only'], ['off', 'Off']]} />
        </SettingRow>
        <SettingRow title="A download finished">
          <Switch label="A download finished" checked={n.onComplete} onChange={(v) => set({ onComplete: v })} />
        </SettingRow>
        <SettingRow title="A download failed">
          <Switch label="A download failed" checked={n.onFailure} onChange={(v) => set({ onFailure: v })} />
        </SettingRow>
        <SettingRow title="A request waits for you" desc="Kind, count and host only: never file names, addresses or an agent's words, which anyone could imitate.">
          <Switch label="A request waits" checked={n.onApproval} onChange={(v) => set({ onApproval: v })} />
        </SettingRow>
        <SettingRow title="A queue started or finished">
          <Switch label="A queue started or finished" checked={n.onQueue} onChange={(v) => set({ onQueue: v })} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Quiet hours">
        <SettingRow title="Quiet hours" desc="Only failures and requests are shown.">
          <Switch label="Quiet hours" checked={n.quietEnabled} onChange={(v) => set({ quietEnabled: v })} />
          {n.quietEnabled ? (
            <>
              <input class="mono time" aria-label="Quiet from" value={n.quietFrom} onChange={(e) => set({ quietFrom: (e.target as HTMLInputElement).value })} />
              <span class="muted">to</span>
              <input class="mono time" aria-label="Quiet to" value={n.quietTo} onChange={(e) => set({ quietTo: (e.target as HTMLInputElement).value })} />
            </>
          ) : null}
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Sounds">
        <SettingRow title="A short tone on finish and failure" desc="Drawn by the app; no sound file is fetched.">
          <Switch label="Sounds" checked={p.sounds} onChange={(v) => setPrefs({ sounds: v })} />
        </SettingRow>
      </SettingsGroup>
    </>
  )
}

/** The desktop app's Search and index page reads and writes the engine's own index configuration. */
export function IndexSettings() {
  return NATIVE ? <NativeIndexSettings /> : <SampleIndexSettings />
}

function SampleIndexSettings() {
  const ix = useStore((s) => s.settings.index)
  const state = useStore((s) => s.index)
  const files = useStore((s) => s.files.length)
  const set = (patch: Partial<typeof ix>) => updateSettings('index', patch)
  const [root, setRoot] = useState('')
  const [excl, setExcl] = useState(ix.exclusions.join('\n'))
  const kinds: [string, string][] = [
    ['ntfs', 'NTFS volume'],
    ['refs', 'ReFS volume'],
    ['folder', 'Folder'],
    ['network', 'Network share'],
  ]
  return (
    <>
      <PageHead title="Search and index" sub="What the file search covers. Only folders you include are indexed; reading a folder never lets Zeus write to it." />
      <SettingsGroup title="Index">
        <SettingRow title="State" desc={state.state === 'scanning' ? `Scanning ${state.scanning ?? ''}: ${Math.round(state.progress * 100)} %` : `Current: generation ${state.generation}, ${files.toLocaleString('en-US')} entries, last full scan ${fmtWhen(state.lastScan)}.`}>
          <button type="button" class="btn small" disabled={state.state === 'scanning'} onClick={() => rescan()}>
            <Icon name="refresh" /> Rescan all
          </button>
        </SettingRow>
        <SettingRow title="Index file size">
          <Switch label="Index file size" checked={ix.indexSize} onChange={(v) => set({ indexSize: v })} />
        </SettingRow>
        <SettingRow title="Index date modified">
          <Switch label="Index date modified" checked={ix.indexDate} onChange={(v) => set({ indexDate: v })} />
        </SettingRow>
        <SettingRow title="Fast sorting" desc="Keeps sorted lists by size and date; uses more memory.">
          <Switch label="Fast sorting" checked={ix.fastSort} onChange={(v) => set({ fastSort: v })} />
        </SettingRow>
        <SettingRow title="File contents" desc="Searching inside files is a separate, cancellable job and is not part of this release." planned>
          <Switch label="File contents" checked={ix.contentIndex} onChange={(v) => set({ contentIndex: v })} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Included folders and volumes" note="An NTFS volume can follow its change journal; ReFS, folders and network shares are rescanned. A network share that goes offline marks its results stale.">
        {ix.roots.map((r, i) => (
          <SettingRow title={<span class="mono">{r.path}</span>} desc={`${kinds.find((k) => k[0] === r.kind)?.[1]}${r.monitorChanges ? ' · follows changes' : ''}${r.rescanEveryHours ? ` · rescan every ${r.rescanEveryHours} h` : ''}`}>
            <Select label="Kind" value={r.kind} onChange={(v) => set({ roots: ix.roots.map((x, j) => (j === i ? { ...x, kind: v as typeof r.kind } : x)) })} options={kinds} />
            <Switch label={`Include ${r.path}`} checked={r.include} onChange={(v) => set({ roots: ix.roots.map((x, j) => (j === i ? { ...x, include: v } : x)) })} />
            <button type="button" class="btn small ghost" onClick={() => rescan(r.path)}>
              Rescan
            </button>
          </SettingRow>
        ))}
        <div class="row">
          <div class="text">
            <input class="mono" aria-label="Folder to include" placeholder="D:\\Datasets" value={root} onInput={(e) => setRoot((e.target as HTMLInputElement).value)} />
          </div>
          <div class="control">
            <button type="button" class="btn small" disabled={!root.trim()} onClick={() => (set({ roots: [...ix.roots, { path: root.trim(), kind: root.startsWith('\\\\') ? 'network' : 'folder', include: true, monitorChanges: false, rescanEveryHours: 24 }] }), setRoot(''))}>
              <Icon name="plus" /> Include folder
            </button>
          </div>
        </div>
      </SettingsGroup>
      <SettingsGroup title="Exclude">
        <SettingRow title="Hidden files">
          <Switch label="Exclude hidden files" checked={ix.excludeHidden} onChange={(v) => set({ excludeHidden: v })} />
        </SettingRow>
        <SettingRow title="System files">
          <Switch label="Exclude system files" checked={ix.excludeSystem} onChange={(v) => set({ excludeSystem: v })} />
        </SettingRow>
        <SettingRow title="Names and folders to skip" desc="One per line; * and ? allowed." stack>
          <textarea class="mono" rows={4} aria-label="Exclusions" value={excl} onInput={(e) => setExcl((e.target as HTMLTextAreaElement).value)} onBlur={() => set({ exclusions: excl.split('\n').map((x) => x.trim()).filter(Boolean) })} />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="History">
        <SettingRow title="Searches to remember">
          <NumberInput label="Searches to remember" value={ix.maxHistory} min={0} max={500} onChange={(v) => set({ maxHistory: v })} />
        </SettingRow>
      </SettingsGroup>
      <Provenance>zeusd has no file index yet; the planned design indexes only folders you include, by stable file identity, with a change journal where the system offers one.</Provenance>
    </>
  )
}

export function AgentSettings() {
  const a = useStore((s) => s.settings.agent)
  const set = (patch: Partial<typeof a>) => updateSettings('agent', patch)
  const [instr, setInstr] = useState(a.instructions)
  return (
    <>
      <PageHead title="Agent" sub="What agent threads may do. The same limits apply to any MCP client of Zeus." />
      <SettingsGroup title="Defaults">
        <SettingRow title="Access for a new thread" desc="A thread never raises its own access; you do, in the thread.">
          <Select<AgentAccess> label="Default access" value={a.defaultAccess} onChange={(v) => set({ defaultAccess: v })} options={ACCESS_ORDER.map((x) => [x, ACCESS_INFO[x].label] as [AgentAccess, string])} />
        </SettingRow>
        <SettingRow title="Largest batch" desc="zeus_enqueue takes 1 to 50 addresses per call.">
          <NumberInput label="Largest batch" value={a.maxBatch} min={1} max={50} onChange={(v) => set({ maxBatch: v })} unit="files" />
        </SettingRow>
        <SettingRow title="Tool calls per reply">
          <NumberInput label="Tool calls per reply" value={a.maxTurnTools} min={1} max={50} onChange={(v) => set({ maxTurnTools: v })} />
        </SettingRow>
        {NATIVE ? (
          <SettingRow title="Trim what the model sees" desc="Tool results are cut to names, sizes and states, with long text shortened. Zeus never reads file contents for a model either way, and addresses lose their query strings.">
            <Switch label="Trim tool results" checked={a.metadataOnly} onChange={(v) => set({ metadataOnly: v })} />
          </SettingRow>
        ) : (
          <SettingRow title="Models see metadata only" desc="Names, sizes and states; never file contents or full addresses." fixed>
            <Chip tone="info">metadata only</Chip>
          </SettingRow>
        )}
      </SettingsGroup>
      <SettingsGroup title="Instructions" note="Added to every thread. They can narrow what the agent does, never widen it.">
        <div class="row stack">
          <textarea class="mono" rows={7} aria-label="Instructions" value={instr} onInput={(e) => setInstr((e.target as HTMLTextAreaElement).value)} onBlur={() => set({ instructions: instr })} />
        </div>
      </SettingsGroup>
      <SettingsGroup title="Tools" note="Switching a tool off refuses every call to it, with the reason shown in the thread.">
        {TOOLS.map((t) => (
          <SettingRow title={<span class="mono">{t.name}</span>} desc={`${t.summary}. Needs ${ACCESS_INFO[t.access].label}.`}>
            <Switch label={`${t.name} enabled`} checked={a.toolsEnabled.includes(t.name)} onChange={(v) => set({ toolsEnabled: v ? [...a.toolsEnabled, t.name] : a.toolsEnabled.filter((x) => x !== t.name) })} />
          </SettingRow>
        ))}
      </SettingsGroup>
      <SettingsGroup title="Never, at any access">
        {NEVER.map((n) => (
          <SettingRow title={n} fixed />
        ))}
      </SettingsGroup>
      <SettingsGroup title="Use Zeus from your own agent">
        <div class="row stack">
          <div class="desc">Any MCP client gets the same thirteen tools over stdio. Register the server once:</div>
          <CodeCopy text="claude mcp add zeus -- zeus mcp" />
        </div>
      </SettingsGroup>
    </>
  )
}

export function StorageSettings() {
  const st = useStore((s) => s.settings.storage)
  const jobs = useStore((s) => s.jobs)
  const set = (patch: Partial<typeof st>) => updateSettings('storage', patch)
  const [preview, setPreview] = useState(false)
  const canceled = jobs.filter((j) => j.state === 'canceled')
  const failed = jobs.filter((j) => j.state === 'failed' && j.done > 0)
  const finishedOld = jobs.filter((j) => j.state === 'completed' && j.finished !== undefined && Date.now() - j.finished > st.keepTombstonesDays * 86_400_000)
  return (
    <>
      <PageHead title="Storage and cleanup" sub="What Zeus keeps and for how long. A cleanup shows what it would remove before it removes anything." />
      <SettingsGroup title="Keep">
        <SettingRow title="Records of removed downloads" zeusd="tombstone_days">
          <NumberInput label="Records" value={st.keepTombstonesDays} min={1} max={365} onChange={(v) => set({ keepTombstonesDays: v })} unit="days" />
        </SettingRow>
        <SettingRow title="Partial data of failed downloads" desc="Kept so a retry can continue.">
          <NumberInput label="Partial data" value={st.clearPartialAfterDays} min={1} max={365} onChange={(v) => set({ clearPartialAfterDays: v })} unit="days" />
        </SettingRow>
        <SettingRow title="Zeus trash" desc="Replaced and removed files wait here before they are gone.">
          <NumberInput label="Trash" value={st.trashRetentionDays} min={1} max={365} onChange={(v) => set({ trashRetentionDays: v })} unit="days" />
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Cleanup">
        <SettingRow title="Preview a cleanup" desc="Lists exactly what would go. Your finished files are never part of a cleanup.">
          <button type="button" class="btn small" onClick={() => setPreview(true)}>
            <Icon name="eye" /> Preview
          </button>
        </SettingRow>
        {preview ? (
          <div class="row stack">
            <ul class="plain">
              <li>{canceled.length} records of canceled downloads (no data left to remove)</li>
              <li>
                {failed.length} partial file{failed.length === 1 ? '' : 's'} of failed downloads, {fmtBytes(failed.reduce((n, j) => n + j.done, 0))}: kept until {st.clearPartialAfterDays} days old
              </li>
              <li>{finishedOld.length} records of finished downloads older than {st.keepTombstonesDays} days; the files themselves stay</li>
            </ul>
            <Notice>Nothing was removed. Removing runs in zeusd with its audit record; this release only previews.</Notice>
          </div>
        ) : null}
      </SettingsGroup>
    </>
  )
}

export function SecuritySettings() {
  const stop = useStore((s) => s.killSwitch)
  return (
    <>
      <PageHead title="Security" sub="The rules Zeus keeps whatever the settings, the instructions or a page say." />
      <SettingsGroup title="Global stop" note="Stopping pauses every transfer and revokes every site permission at once. Releasing it restores neither: you resume what you want and approve sites again.">
        <SettingRow title="Global stop" desc={stop ? 'On: everything is paused.' : 'Off.'}>
          {stop ? (
            <button type="button" class="btn" onClick={releaseStop}>
              Release
            </button>
          ) : (
            <button type="button" class="btn danger" onClick={() => openDialog({ kind: 'stop' })}>
              <Icon name="stop" /> Stop everything
            </button>
          )}
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Only a person decides">
        {HUMAN_ONLY.map((h) => (
          <SettingRow title={h.title} desc={h.desc} fixed />
        ))}
      </SettingsGroup>
      <SettingsGroup title="Fixed rules">
        <SettingRow title="No agent channel can approve" desc="MCP, A2A and bridges cannot decide an approval, create or widen a grant, release the global stop or change policy (Zeus ADR 0007)." fixed />
        <SettingRow title="Zeus never reads browser cookie stores" desc="Sessions come only through the extension and native messaging (ADR 0004)." fixed />
        <SettingRow title="Secrets stay out of logs" desc="Cookies, tokens, keys and URL query strings never reach logs, events, errors or a model prompt." fixed />
        <SettingRow title="Downloaded files are never opened automatically" desc="Programs, scripts and disguised files are quarantined; the system's download mark is applied to every file." fixed />
        <SettingRow title="Approve buttons arm after 750 ms" desc="And again after any change of the facts, so a stray click or type-ahead cannot decide." fixed />
        <SettingRow title="One local listener" desc="zeusd listens only on its local endpoint; HTTP surfaces are separate bridge processes (ADR 0010)." fixed />
        <SettingRow title="No telemetry" desc="The only network traffic is what a download, a workflow or a configured provider causes." fixed />
      </SettingsGroup>
    </>
  )
}

export function DaemonSettings() {
  return (
    <>
      <PageHead title="Daemon and environments" sub="Which zeusd this app talks to." />
      <SettingsGroup title="This app">
        <SettingRow title="Backend" desc="This pre-alpha release runs on sample data inside the app. It cannot link to a real zeusd.">
          <Chip tone="warn">sample data, no daemon</Chip>
        </SettingRow>
        <SettingRow title="Link to a zeusd on this computer" desc="Not available in this release." planned>
          <button type="button" class="btn small" disabled>
            Link
          </button>
        </SettingRow>
      </SettingsGroup>
      <SettingsGroup title="Using a real zeusd today" note="The separate Zeus desktop client in the zeus repository (apps/desktop) talks to a real zeusd with a restricted credential: downloads, link review, workflow reports, files of the default folder and artifact facts. It does not decide approvals.">
        <div class="row stack">
          <CodeCopy text={'cargo build -p zeus-daemon -p zeus-cli -p zeus-native-host\ncargo run -p zeus-cli -- daemon install\ncargo run -p zeus-daemon --bin zeusd -- run\ncargo run --manifest-path apps/desktop/Cargo.toml'} />
        </div>
      </SettingsGroup>
      <SettingsGroup title="Elsewhere">
        <SettingRow title="Another computer" desc="zeusd has one local listener and no remote control; a phone or another computer would need a separately designed, authenticated transport." planned />
        <SettingRow title="Android and iOS" desc="No mobile app exists. Phones need native background-transfer and storage adapters first." planned />
      </SettingsGroup>
    </>
  )
}

