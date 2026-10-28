// Providers in the desktop app: the language model ladder with the person's own keys.
//
// Models are optional: downloading never needs one. A provider is called only after the person allows it, never while
// Offline is on, and never beyond its cap. The key is typed here once, goes to the engine in one request, and comes back
// only as "stored" with its last four characters; it is kept in the operating system's credential store.

import { useEffect, useRef, useState } from 'preact/hooks'

import type { AgentConfigWrite } from '../../backend/agentNative'
import { agentApi, providerBlocker, refreshConfig, setConfig, toWrite } from '../../backend/agentNative'
import { toast } from '../../backend/store'
import { useAgentConfig } from '../../backend/useAgentConfig'
import { Chip, NumberInput, PageHead, Provenance, Select, SettingRow, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import '../../styles/agent-native.css'

type Prov = AgentConfigWrite['providers'][number]

function usd(v: number): string {
  return `$${v.toFixed(v < 1 ? 4 : 2)}`
}

export function NativeProviders() {
  const cfg = useAgentConfig()
  const [draft, setDraft] = useState<AgentConfigWrite | null>(null)
  const [sel, setSel] = useState(0)
  const [adding, setAdding] = useState(false)
  const [keyInput, setKeyInput] = useState('')
  const [models, setModels] = useState<string[]>([])
  const [busy, setBusy] = useState<string | null>(null)
  const [result, setResult] = useState<{ ok: boolean; text: string } | null>(null)
  const [pass, setPass] = useState('')
  const timer = useRef<ReturnType<typeof setTimeout> | undefined>(undefined)

  useEffect(() => {
    if (cfg && !draft) setDraft(toWrite(cfg))
  }, [cfg, draft])

  if (!cfg || !draft) return <PageHead title="Providers" sub="Starting the engine…" />
  const view = cfg.providers.find((p) => p.id === draft.providers[sel]?.id)
  const cur = draft.providers[sel]
  const subscription = ['codex', 'claude', 'cursor', 'antigravity'].includes(cur?.kind ?? '')

  const save = (next: AgentConfigWrite) => {
    setDraft(next)
    if (timer.current) clearTimeout(timer.current)
    timer.current = setTimeout(() => {
      agentApi.putConfig(next).then(setConfig, (e: unknown) => toast({ kind: 'bad', title: 'Not saved', desc: e instanceof Error ? e.message : 'The engine refused these settings.', important: true, topic: 'system' }))
    }, 450)
  }
  const edit = (patch: Partial<Prov>) => save({ ...draft, providers: draft.providers.map((p, i) => (i === sel ? { ...p, ...patch } : p)) })
  const move = (d: -1 | 1) => {
    const k = sel + d
    if (k < 0 || k >= draft.providers.length) return
    const n = [...draft.providers]
    ;[n[sel], n[k]] = [n[k] as Prov, n[sel] as Prov]
    save({ ...draft, providers: n })
    setSel(k)
  }
  const run = async (what: string, f: () => Promise<void>) => {
    setBusy(what)
    try {
      await f()
    } catch (e) {
      setResult({ ok: false, text: e instanceof Error ? e.message : 'That did not work.' })
    } finally {
      setBusy(null)
    }
  }
  const flushThen = async (f: () => Promise<void>) => {
    if (timer.current) clearTimeout(timer.current)
    setConfig(await agentApi.putConfig(draft))
    await f()
  }
  const month = view?.month.usd ?? 0
  const pct = cur && cur.monthly_cap_usd > 0 ? Math.min(100, (month / cur.monthly_cap_usd) * 100) : 100
  const blocker = view ? providerBlocker(cfg, view) : null
  const ks = cfg.keystore

  return (
    <>
      <PageHead title="Providers" sub="Use an existing agent sign-in or an API key. Installed agents use their own account and plan limits. Files and transfers work without a model." />
      <SettingsGroup title="Safety switches" note="Consent and Offline apply to every connection. API spending caps apply to API-key providers. Installed agents use their provider’s plan limits and extra-usage settings. A cap of zero disables calls.">
        <SettingRow title="Offline" desc="No model is called at all, local ones included. The agent says so instead of trying.">
          <Switch label="Offline" checked={draft.offline} onChange={(v) => save({ ...draft, offline: v })} />
        </SettingRow>
        <SettingRow title="Spending cap per day" desc={`Over all providers. Spent today: about ${usd(cfg.usage.today.usd)}.`}>
          <NumberInput label="Daily cap in US dollars" value={draft.daily_cap_usd} min={0} max={100000} step={0.5} unit="USD" onChange={(v) => save({ ...draft, daily_cap_usd: v })} />
        </SettingRow>
        <SettingRow title="Spending cap per thread">
          <NumberInput label="Thread cap in US dollars" value={draft.thread_cap_usd} min={0} max={100000} step={0.5} unit="USD" onChange={(v) => save({ ...draft, thread_cap_usd: v })} />
        </SettingRow>
        <SettingRow title="Longest answer" desc="The output limit of one model call.">
          <NumberInput label="Output limit in tokens" value={draft.max_tokens} min={64} max={16384} step={64} unit="tokens" onChange={(v) => save({ ...draft, max_tokens: Math.round(v) })} />
        </SettingRow>
        <SettingRow title="Price when a provider has none" desc="Dollars per million tokens, in and out. Estimates for the caps and the meter; check your provider's price list. The default is deliberately high so a cap trips early.">
          <span class="price-grid">
            <NumberInput label="Default input price per million tokens" value={draft.default_price_in_per_mtok} min={0} max={100000} step={0.5} unit="in" onChange={(v) => save({ ...draft, default_price_in_per_mtok: v })} />
            <NumberInput label="Default output price per million tokens" value={draft.default_price_out_per_mtok} min={0} max={100000} step={0.5} unit="out" onChange={(v) => save({ ...draft, default_price_out_per_mtok: v })} />
          </span>
        </SettingRow>
      </SettingsGroup>

      <SettingsGroup title="Where keys are kept" note="Keys are stored by Zeus's engine process and never reach this page, a log, a URL or a prompt. This page only learns 'stored' and the last four characters.">
        <div class="row stack keystore-box">
          <div>
            <Chip tone={ks.backend === 'os' ? 'ok' : 'info'}>{ks.backend === 'os' ? "this computer's credential store" : 'an encrypted file'}</Chip>{' '}
            <span class="muted small">{ks.backend === 'os' ? 'Windows Credential Manager, the Secret Service on Linux, or the Keychain.' : 'No credential store was found here. Keys are sealed in a file under a passphrase you type; it is held in memory only.'}</span>
          </div>
          {ks.backend === 'file' ? (
            <div class="inline">
              {!ks.initialized ? (
                <>
                  <input type="password" autocomplete="off" aria-label="New passphrase" placeholder="choose a passphrase (8 characters or more)" value={pass} onInput={(e) => setPass((e.target as HTMLInputElement).value)} />
                  <button type="button" class="btn small primary" disabled={pass.length < 8 || busy !== null} onClick={() => void run('init', async () => (setConfig({ ...cfg, keystore: await agentApi.initKeystore(pass) }), setPass('')))}>
                    Create key file
                  </button>
                </>
              ) : ks.unlocked ? (
                <>
                  <Chip tone="ok">unlocked until Zeus closes</Chip>
                  <button type="button" class="btn small" onClick={() => void run('lock', async () => setConfig({ ...cfg, keystore: await agentApi.lockKeystore() }))}>
                    Lock now
                  </button>
                </>
              ) : (
                <>
                  <input type="password" autocomplete="off" aria-label="Passphrase" placeholder="passphrase of the key file" value={pass} onInput={(e) => setPass((e.target as HTMLInputElement).value)} />
                  <button type="button" class="btn small primary" disabled={!pass || busy !== null} onClick={() => void run('unlock', async () => (setConfig({ ...cfg, keystore: await agentApi.unlockKeystore(pass) }), setPass('')))}>
                    Unlock
                  </button>
                </>
              )}
            </div>
          ) : null}
        </div>
      </SettingsGroup>

      <section class="sgroup">
        <div class="sgroup-head">
          <h2 class="sgroup-title">The ladder</h2>
          <div class="sgroup-actions">
            <button type="button" class="btn small" aria-expanded={adding} onClick={() => setAdding((v) => !v)}>
              <Icon name="plus" /> Add provider
            </button>
          </div>
        </div>
        {adding ? (
          <div class="preset-grid">
            {cfg.kinds.map((k) => (
              <button
                type="button"
                class="preset"
                onClick={() => {
                  const id = `p-${k.kind}-${Date.now().toString(36).slice(-4)}`
                  save({ ...draft, providers: [...draft.providers, { id, name: k.label, kind: k.kind, enabled: false, base_url: '', model: ['codex', 'claude', 'cursor', 'antigravity'].includes(k.kind) ? 'default' : '', consent: false, monthly_cap_usd: 10, tools_mode: 'native', price_in_per_mtok: ['ollama', 'codex', 'claude', 'cursor', 'antigravity'].includes(k.kind) ? 0 : null, price_out_per_mtok: ['ollama', 'codex', 'claude', 'cursor', 'antigravity'].includes(k.kind) ? 0 : null }] })
                  setSel(draft.providers.length)
                  setAdding(false)
                }}
              >
                <span class="preset-name">{k.label}</span>
                <span class="preset-meta">
                  <span class="mono">{['codex', 'claude', 'cursor', 'antigravity'].includes(k.kind) ? 'Uses your installed agent sign-in' : k.default_base_url ? k.default_base_url.replace(/^https?:\/\//, '') : 'your own address'}</span>
                  {k.needs_key ? null : <Chip tone="ok">no key needed</Chip>}
                </span>
              </button>
            ))}
          </div>
        ) : null}
        <div class="rows split-pane">
          <div class="pane-list" role="listbox" aria-label="Providers, in the order they are tried">
            {draft.providers.map((p, i) => {
              const v = cfg.providers.find((x) => x.id === p.id)
              const b = v ? providerBlocker({ ...cfg, offline: draft.offline }, { ...v, ...p, key: v.key }) : null
              return (
                <div class={`pane-item ${i === sel ? 'on' : ''}`} role="option" aria-selected={i === sel} tabIndex={0} onClick={() => (setSel(i), setKeyInput(''), setModels([]), setResult(null))} onKeyDown={(e) => e.key === 'Enter' && setSel(i)}>
                  <span class="ord">{i + 1}</span>
                  <span class="pi-text">
                    <span class="pi-name">{p.name}</span>
                    <span class="pi-meta mono">
                      {p.kind} · {p.model || 'no model'} · {v?.key.stored ? `key ••••${v.key.last4 ?? ''}` : v && !v.needs_key ? 'no key needed' : 'no key'}
                      {p.enabled && b ? ` · ${b.text}` : ''}
                    </span>
                  </span>
                  <Switch label={`${p.name} enabled`} checked={p.enabled} onChange={(on) => save({ ...draft, providers: draft.providers.map((x, j) => (j === i ? { ...x, enabled: on } : x)) })} />
                </div>
              )
            })}
            {!draft.providers.length ? <div class="pane-empty">No provider. No model is ever called.</div> : null}
          </div>
          <div class="pane-detail">
            {cur && view ? (
              <>
                <div class="pane-head">
                  <div>
                    <div class="title">{cur.name}</div>
                    <div class="desc">{!cur.enabled ? 'Switched off: never called.' : blocker ? `Not ready: ${blocker.text}.` : `Ready, tried in position ${sel + 1}.`}</div>
                  </div>
                  <div class="pane-tools">
                    <button type="button" class="icon-btn" aria-label="Move up" disabled={sel === 0} onClick={() => move(-1)}>
                      <Icon name="up" />
                    </button>
                    <button type="button" class="icon-btn" aria-label="Move down" disabled={sel === draft.providers.length - 1} onClick={() => move(1)}>
                      <Icon name="down" />
                    </button>
                    <button
                      type="button"
                      class="icon-btn"
                      aria-label="Remove provider"
                      onClick={() => {
                        void agentApi.deleteKey(cur.id).catch(() => undefined)
                        save({ ...draft, providers: draft.providers.filter((_, j) => j !== sel) })
                        setSel(Math.max(0, sel - 1))
                      }}
                    >
                      <Icon name="trash" />
                    </button>
                  </div>
                </div>
                <div class="fields">
                  <label class="field">
                    Name
                    <input value={cur.name} onInput={(e) => edit({ name: (e.target as HTMLInputElement).value })} />
                  </label>
                  <label class="field">
                    Kind
                    <input class="mono" value={cur.kind} disabled />
                  </label>
                  {!subscription ? (<label class="field wide">
                    Base URL <span class="muted small">https, or http to this computer or your own network</span>
                    <input class="mono" value={cur.base_url} placeholder={view.effective_base_url || 'https://…'} onInput={(e) => edit({ base_url: (e.target as HTMLInputElement).value.trim() })} />
                  </label>) : <p class="muted small">Uses the installed agent’s sign-in. Sign in with that provider first. Your plan controls model access, quotas and any extra usage.</p>}
                  <label class="field wide">
                    Model
                    <span class="inline">
                      <input class="mono" list={`models-${cur.id}`} value={cur.model} placeholder="the model id, as the provider names it" onInput={(e) => edit({ model: (e.target as HTMLInputElement).value.trim() })} />
                      <datalist id={`models-${cur.id}`}>
                        {models.map((m) => (
                          <option value={m} />
                        ))}
                      </datalist>
                      <button
                        type="button"
                        class="btn small"
                        disabled={busy !== null}
                        title="Asks the provider which models it offers (sends only your key)"
                        onClick={() =>
                          void run('models', () =>
                            flushThen(async () => {
                              const r = await agentApi.models(cur.id)
                              setModels(r.models ?? [])
                              setResult(r.ok ? { ok: true, text: `${r.models?.length ?? 0} models listed. Pick one in the box.` } : { ok: false, text: r.message ?? 'The provider did not list models.' })
                            }),
                          )
                        }
                      >
                        {busy === 'models' ? 'Asking…' : 'List models'}
                      </button>
                    </span>
                  </label>
                  <label class="field">
                    Tools
                    <Select<'native' | 'json'> label="How tools are offered" value={cur.tools_mode} onChange={(v) => edit({ tools_mode: v })} options={[['native', "The provider's function calling"], ['json', 'Described in the prompt (models without tool calling)']]} />
                  </label>
                  {!subscription && (<div class="field wide">
                    API key {view.needs_key ? <span class="req">needed</span> : <span class="muted">not needed</span>}
                    <span class="inline">
                      <input type="password" autocomplete="off" spellcheck={false} aria-label="API key" placeholder={view.key.stored ? `stored · ••••${view.key.last4 ?? ''}` : 'paste a key'} value={keyInput} onInput={(e) => setKeyInput((e.target as HTMLInputElement).value)} />
                      <button
                        type="button"
                        class="btn small primary"
                        disabled={keyInput.trim().length < 8 || busy !== null}
                        onClick={() =>
                          void run('key', () =>
                            flushThen(async () => {
                              await agentApi.putKey(cur.id, keyInput.trim())
                              setKeyInput('')
                              await refreshConfig()
                              setResult({ ok: true, text: 'The key is stored by the engine. This page keeps nothing of it.' })
                            }),
                          )
                        }
                      >
                        Store key
                      </button>
                      {view.key.stored ? (
                        <button type="button" class="btn small ghost" onClick={() => void run('delkey', async () => (await agentApi.deleteKey(cur.id), await refreshConfig(), setResult({ ok: true, text: 'The key was removed.' })))}>
                          Remove
                        </button>
                      ) : null}
                      {view.key.stored ? <Chip tone="ok">stored · ••••{view.key.last4 ?? ''}</Chip> : null}
                    </span>
                    <span class="muted small">Sent only to {view.effective_base_url.replace(/^https?:\/\//, '').split('/')[0]}, in the request header, by the engine.</span>
                  </div>)}
                  <div class="field wide">
                    Permission
                    <span class="inline">
                      <Switch label={`Allow Zeus to send threads to ${cur.name}`} checked={cur.consent} onChange={(v) => edit({ consent: v })} />
                      <span class="muted small">
                        {cur.consent ? `Zeus may send threads to ${cur.name}. Each thread's first request shows what will be sent.` : `Off. The first request of a thread shows exactly what would be sent and asks.`}
                      </span>
                    </span>
                  </div>
                  {!subscription ? <>                  <label class="field">
                    Monthly spending cap
                    <NumberInput label="Monthly cap in US dollars" value={cur.monthly_cap_usd} min={0} max={100000} step={0.5} unit="USD" onChange={(v) => edit({ monthly_cap_usd: v })} />
                  </label>
                  <div class="field">
                    Prices per million tokens
                    <span class="price-grid">
                      <input class="mono" type="number" min={0} step={0.1} aria-label="Input price per million tokens" placeholder={`in ${view.effective_price_in_per_mtok}`} value={cur.price_in_per_mtok ?? ''} onChange={(e) => edit({ price_in_per_mtok: (e.target as HTMLInputElement).value === '' ? null : Number((e.target as HTMLInputElement).value) })} />
                      <input class="mono" type="number" min={0} step={0.1} aria-label="Output price per million tokens" placeholder={`out ${view.effective_price_out_per_mtok}`} value={cur.price_out_per_mtok ?? ''} onChange={(e) => edit({ price_out_per_mtok: (e.target as HTMLInputElement).value === '' ? null : Number((e.target as HTMLInputElement).value) })} />
                    </span>
                  </div>
                  <div class="field wide spend-meter" aria-label="Spent this month">
                    <span>
                      This month: about {usd(month)} of {usd(cur.monthly_cap_usd)} · {view.month.calls} calls, {view.month.input_tokens.toLocaleString()} tokens in, {view.month.output_tokens.toLocaleString()} out
                    </span>
                    <span class={`bar ${pct > 80 ? 'hot' : ''}`}>
                      <i style={{ width: `${pct}%` }} />
                    </span>
                  </div></> : <p class="muted small">Subscription calls are counted when the CLI reports usage. API dollar caps do not limit charges made through that provider’s own account.</p>}
                  <div class="field wide">
                    <span class="inline">
                      <button
                        type="button"
                        class="btn small"
                        disabled={busy !== null}
                        title="One tiny real request: it needs your permission, a key and a model"
                        onClick={() =>
                          void run('test', () =>
                            flushThen(async () => {
                              const r = await agentApi.test(cur.id)
                              setResult(r.ok ? { ok: true, text: `Answered in ${r.ms} ms by ${r.model}${r.text ? `: "${r.text}"` : ''}. Cost about ${usd(r.usd ?? 0)}.` } : { ok: false, text: r.message ?? 'The test did not work.' })
                            }),
                          )
                        }
                      >
                        {busy === 'test' ? 'Testing…' : 'Test connection'}
                      </button>
                      {result ? <span class={`test-result ${result.ok ? 'ok' : 'bad'}`}>{result.text}</span> : null}
                    </span>
                  </div>
                </div>
              </>
            ) : (
              <div class="pane-empty">Choose a provider, or add one.</div>
            )}
          </div>
        </div>
      </section>
      <Provenance>The ladder, your keys and the caps belong to this app's engine. The same fourteen tools any MCP client of Zeus gets are all the agent can use, and none of them decides an approval.</Provenance>
    </>
  )
}
