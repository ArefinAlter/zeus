// Browser integration: the Zeus extension, the native-messaging host that links it to this app's engine, and what each
// browser on this computer can do with them. Nothing is written until the person has read the list and consented.

import { useEffect, useState } from 'preact/hooks'

import type { BrowserState, InstallPlan, IntegrationStatus } from '../../backend/integration'
import { integration } from '../../backend/integration'
import { native } from '../../backend/remote'
import { toast } from '../../backend/store'
import { Chip, CodeCopy, CopyButton, Dialog, Kv, Notice } from '../../ui/components'
import { Icon } from '../../ui/icons'
import '../../styles/integration.css'

function say(e: unknown): string {
  return e instanceof Error ? e.message : 'That did not work.'
}

function ConsentDialog({ plan, onClose, onConfirm, busy }: { plan: InstallPlan; onClose: () => void; onConfirm: () => void; busy: boolean }) {
  const [ok, setOk] = useState(false)
  return (
    <Dialog title="Install browser integration" onClose={onClose} wide>
      <p>
        Zeus will register its native-messaging host for {plan.browsers.join(', ')}, for your user only. This writes exactly the following; nothing outside this list is touched.
      </p>
      <ul class="plan-steps">
        {plan.steps.map((s) => (
          <li>
            <span class="mono small">
              {s.action} {s.target}
              {s.value ? ` = ${s.value}` : ''}
            </span>
            {s.note ? <span class="muted small"> · {s.note}</span> : null}
          </li>
        ))}
      </ul>
      <label class="check-row">
        <input type="checkbox" checked={ok} onChange={(e) => setOk((e.target as HTMLInputElement).checked)} />
        <span>
          I understand this release carries the <strong>development build</strong> of the extension. Its id ({plan.development_extension_ids.join(', ')}) comes from a fixed development key and is listed as a development extension; a store release will have a different id, which is not known yet.
        </span>
      </label>
      <p class="muted small">Zeus never reads your browser's cookies or profile files. Logins reach Zeus only through the extension, after you approve a site in the extension's popup or at a terminal.</p>
      <div class="decide-row">
        <button type="button" class="btn" onClick={onClose}>
          Cancel
        </button>
        <button type="button" class="btn primary" disabled={!ok || busy} onClick={onConfirm}>
          {busy ? 'Installing…' : 'Install'}
        </button>
      </div>
    </Dialog>
  )
}

function Registration({ b }: { b: BrowserState }) {
  if (b.registered && b.points_at_this_app && b.extension_pinned) return <Chip tone="ok">host registered</Chip>
  if (b.registered) return <Chip tone="warn" title="Registered for a different installation or extension id">registered elsewhere</Chip>
  return <Chip>not registered</Chip>
}

export function BrowserIntegration() {
  const [st, setSt] = useState<IntegrationStatus | null>(null)
  const [err, setErr] = useState<string | null>(null)
  const [plan, setPlan] = useState<InstallPlan | null>(null)
  const [busy, setBusy] = useState(false)
  const [picked, setPicked] = useState<string[] | null>(null)
  const [exported, setExported] = useState<string | null>(null)
  const [dir, setDir] = useState('')

  const load = () => integration.status().then((s) => (setSt(s), setErr(null)), (e: unknown) => setErr(say(e)))
  useEffect(() => {
    void load()
    const t = setInterval(() => void load(), 4000)
    return () => clearInterval(t)
  }, [])

  if (!st) return <p class="muted">{err ?? 'Reading the browser integration…'}</p>
  const found = st.browsers.filter((b) => b.installed)
  const chosen = picked ?? found.filter((b) => !b.registered || !b.points_at_this_app).map((b) => b.id)
  const toggle = (id: string) => setPicked(chosen.includes(id) ? chosen.filter((x) => x !== id) : [...chosen, id])
  const registeredHere = st.browsers.filter((b) => b.registered && b.points_at_this_app)
  const showPlan = () => {
    setBusy(true)
    integration
      .plan(chosen)
      .then(setPlan, (e: unknown) => setErr(say(e)))
      .finally(() => setBusy(false))
  }
  const install = () => {
    if (!plan) return
    setBusy(true)
    integration
      .install(plan.browsers, { development_extension_id: true })
      .then(
        () => {
          setPlan(null)
          setPicked(null)
          toast({ kind: 'ok', title: 'Browser integration installed', desc: 'Now load the extension in the browser (steps below).', important: false, topic: 'system' })
          return load()
        },
        (e: unknown) => setErr(say(e)),
      )
      .finally(() => setBusy(false))
  }
  const remove = (ids: string[]) => {
    setBusy(true)
    integration
      .uninstall(ids)
      .then(() => load(), (e: unknown) => setErr(say(e)))
      .finally(() => setBusy(false))
  }
  const open = (path: string) => native.open(path, 'open').catch((e: unknown) => setErr(say(e)))
  return (
    <div class="integration">
      {!st.extension.packaged ? <Notice kind="warn">This build of Zeus does not carry the browser extension, so the browser cannot be connected from here. It is built with apps/client/scripts/package-extension.mjs.</Notice> : null}
      {err ? <Notice kind="error">{err}</Notice> : null}
      <Kv
        rows={[
          ['extension', st.extension.packaged ? `Zeus ${st.extension.meta.version_name ?? st.extension.meta.version ?? ''} (development build)` : 'not in this build'],
          ['connected now', st.extension_connected === null ? 'unknown' : st.extension_connected ? <Chip tone="ok">an extension is connected</Chip> : <Chip>no extension connected</Chip>],
          ['extension id', <span class="mono small">{st.extension.meta.chromium_dev_id ?? '–'}</span>],
          ['host', st.host.exe_present ? <span class="mono small">{st.host.exe}</span> : <Chip tone="bad">missing: reinstall Zeus</Chip>],
        ]}
      />
      <div class="browser-table" role="table" aria-label="Browsers on this computer">
        {st.browsers.map((b) => (
          <div class="browser-row" role="row" key={b.id}>
            <label class="browser-pick">
              <input type="checkbox" disabled={!b.installed} checked={b.installed && chosen.includes(b.id)} onChange={() => toggle(b.id)} aria-label={`Register ${b.name}`} />
              <span class="name">{b.name}</span>
            </label>
            <span class="chips">
              {b.installed ? <Chip tone="ok">found</Chip> : <Chip>not found</Chip>}
              {b.installed ? (b.recognized_by_host ? <Chip tone="ok" title="Zeus can verify this install as the browser that started its host">verifiable</Chip> : <Chip tone="warn" title={st.notes.recognized}>per-user install</Chip>) : null}
              <Registration b={b} />
            </span>
            <span class="actions">
              {b.registered && b.points_at_this_app ? (
                <button type="button" class="btn small ghost" disabled={busy} onClick={() => remove([b.id])}>
                  Remove
                </button>
              ) : null}
            </span>
          </div>
        ))}
      </div>
      {st.browsers.some((b) => b.installed && !b.recognized_by_host) ? <Notice kind="warn">{st.notes.recognized}</Notice> : null}
      <div class="toolbar">
        <button type="button" class="btn primary" disabled={busy || !st.extension.packaged || !st.host.exe_present || !chosen.length} onClick={showPlan}>
          <Icon name="plug" /> Install browser integration
        </button>
        {registeredHere.length ? (
          <button type="button" class="btn" disabled={busy} onClick={() => remove([])}>
            Remove all
          </button>
        ) : null}
      </div>
      {plan ? <ConsentDialog plan={plan} busy={busy} onClose={() => setPlan(null)} onConfirm={install} /> : null}

      {registeredHere.length ? (
        <div class="stack-tight">
          <h3>Load the extension in your browser</h3>
          <p class="muted small">Browsers do not let another program open their extensions page, so this part is yours. It takes a minute.</p>
          {registeredHere.map((b) => {
            const folder = b.family === 'firefox' ? st.extension.firefox_folder : st.extension.chromium_folder
            return (
              <div class="load-steps" key={b.id}>
                <strong>{b.name}</strong>
                {b.family === 'chromium' ? (
                  <ol>
                    <li>
                      Open <span class="mono">{b.extensions_page}</span> <CopyButton text={b.extensions_page} label="Copy" /> and switch on Developer mode.
                    </li>
                    <li>
                      Choose Load unpacked and pick this folder: <span class="mono small">{folder}</span> <CopyButton text={folder} label="Copy path" />
                    </li>
                    <li>The extension's id should read {b.extension_id}. Open its popup: it says "Connected to Zeus" when the engine is running.</li>
                  </ol>
                ) : (
                  <ol>
                    <li>
                      Open <span class="mono">{b.extensions_page}</span> <CopyButton text={b.extensions_page} label="Copy" />, then This Firefox.
                    </li>
                    <li>
                      Choose Load Temporary Add-on and pick <span class="mono small">manifest.json</span> in: <span class="mono small">{folder}</span> <CopyButton text={folder} label="Copy path" />
                    </li>
                    <li class="muted">{st.notes.firefox}</li>
                  </ol>
                )}
                <button type="button" class="btn small" onClick={() => void open(folder)}>
                  <Icon name="folderopen" /> Open the folder
                </button>
              </div>
            )
          })}
        </div>
      ) : null}

      <div class="stack-tight">
        <h3>Keep a copy of the extension</h3>
        <p class="muted small">For a browser or computer that is not listed above, or to load it by hand later. Zeus copies the zip files and the unpacked folders into a folder you name.</p>
        <div class="inline">
          <input class="mono" aria-label="Folder to copy the extension into" placeholder={st.os === 'windows' ? 'C:\\Users\\you\\Desktop' : '/home/you/Desktop'} value={dir} onInput={(e) => setDir((e.target as HTMLInputElement).value)} />
          <button
            type="button"
            class="btn"
            disabled={!dir.trim() || busy || !st.extension.packaged}
            onClick={() => {
              setBusy(true)
              integration
                .exportTo(dir.trim())
                .then((r) => setExported(r.folder), (e: unknown) => setErr(say(e)))
                .finally(() => setBusy(false))
            }}
          >
            Copy here
          </button>
        </div>
        {exported ? <p class="small">Copied to <span class="mono">{exported}</span></p> : null}
      </div>
      <details class="small">
        <summary>What this does and what it does not</summary>
        <p>{st.notes.development_id}</p>
        <p>The browser starts a small launcher script that Zeus writes ({st.host.launcher}); it points the host at this app's engine and then starts the bundled host. The host's own allow-list ({st.host.allow_list}) names the development extension id. Remove all undoes every registration and deletes both files.</p>
        <p>
          The host writes what it could not do to <span class="mono">{st.host.log}</span> (never page content or links).
        </p>
        <CodeCopy text={st.host.dev_extensions.length ? `allowed development extensions: ${st.host.dev_extensions.join(', ')}` : 'no extension is allowed yet'} />
      </details>
    </div>
  )
}
