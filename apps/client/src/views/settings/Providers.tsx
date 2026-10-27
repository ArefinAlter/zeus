// Providers: the language model ladder, bring your own key. Models are optional: downloads never need one. Entries
// are tried in order. A key typed here stays in this page's memory for the session only: it is never written to
// storage, never sent anywhere (this release makes no network request), and shown only as "stored" with its last four
// characters. In zeusd keys live in the OS keyring (zeus auth), and a provider is called only for drafting workflows.

import { useState } from 'preact/hooks'

import type { ProviderEntry } from '../../backend/settings'
import { PROVIDER_PRESETS } from '../../backend/settings'
import { NATIVE } from '../../backend/live'
import { updateSettings, useStore } from '../../backend/store'
import { Chip, CodeCopy, NumberInput, PageHead, Provenance, SettingsGroup, Switch } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { NativeProviders } from './ProvidersNative'

function masked(key: string | undefined): string {
  if (!key) return 'no key'
  return `stored · ••••${key.slice(-4)}`
}

export function ProvidersSettings() {
  return NATIVE ? <NativeProviders /> : <ScriptedProviders />
}

function ScriptedProviders() {
  const list = useStore((s) => s.settings.providers)
  const [sel, setSel] = useState(0)
  const [adding, setAdding] = useState(false)
  const [draftKey, setDraftKey] = useState('')
  const cur = list[sel]
  const save = (next: ProviderEntry[]) => updateSettings('providers', next)
  const edit = (patch: Partial<ProviderEntry>) => save(list.map((p, i) => (i === sel ? { ...p, ...patch } : p)))
  const move = (d: -1 | 1) => {
    const k = sel + d
    if (k < 0 || k >= list.length) return
    const n = [...list]
    ;[n[sel], n[k]] = [n[k] as ProviderEntry, n[sel] as ProviderEntry]
    save(n)
    setSel(k)
  }
  const preset = PROVIDER_PRESETS.find((x) => x.kind === cur?.kind)
  const needsKey = preset?.needsKey ?? false
  return (
    <>
      <PageHead title="Providers" sub="Language models for the agent and for drafting workflows, tried in order. Optional: downloading never needs a model." />
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
            {PROVIDER_PRESETS.map((p) => (
              <button
                type="button"
                class="preset"
                onClick={() => {
                  save([...list, { id: `p-${Date.now().toString(36)}`, name: p.name, kind: p.kind, enabled: false, baseUrl: p.baseUrl, model: '', monthlyCapUsd: p.needsKey ? 10 : 0 }])
                  setSel(list.length)
                  setAdding(false)
                }}
              >
                <span class="preset-name">{p.name}</span>
                <span class="preset-meta">
                  <span class="mono">{p.baseUrl.replace(/^https?:\/\//, '')}</span>
                  {p.needsKey ? null : <Chip tone="ok">no key</Chip>}
                </span>
              </button>
            ))}
          </div>
        ) : null}
        <div class="rows split-pane">
          <div class="pane-list" role="listbox" aria-label="Providers, in the order they are tried">
            {list.map((p, i) => (
              <div class={`pane-item ${i === sel ? 'on' : ''}`} role="option" aria-selected={i === sel} tabIndex={0} onClick={() => (setSel(i), setDraftKey(''))} onKeyDown={(e) => e.key === 'Enter' && setSel(i)}>
                <span class="ord">{i + 1}</span>
                <span class="pi-text">
                  <span class="pi-name">{p.name}</span>
                  <span class="pi-meta mono">
                    {p.kind}
                    {p.model ? ` · ${p.model}` : ' · no model'} · {masked(p.key)}
                  </span>
                </span>
                <Switch label={`${p.name} enabled`} checked={p.enabled} onChange={(on) => save(list.map((x, j) => (j === i ? { ...x, enabled: on } : x)))} />
              </div>
            ))}
            {!list.length ? <div class="pane-empty">No provider. No model is ever called.</div> : null}
          </div>
          <div class="pane-detail">
            {cur ? (
              <>
                <div class="pane-head">
                  <div>
                    <div class="title">{cur.name}</div>
                    <div class="desc">{cur.enabled ? (cur.model ? `Tried in position ${sel + 1}.` : 'Enabled, but no model is named yet.') : 'Disabled: never called.'}</div>
                  </div>
                  <div class="pane-tools">
                    <button type="button" class="icon-btn" aria-label="Move up" disabled={sel === 0} onClick={() => move(-1)}>
                      <Icon name="up" />
                    </button>
                    <button type="button" class="icon-btn" aria-label="Move down" disabled={sel === list.length - 1} onClick={() => move(1)}>
                      <Icon name="down" />
                    </button>
                    <button type="button" class="icon-btn" aria-label="Remove provider" onClick={() => (save(list.filter((_, j) => j !== sel)), setSel(Math.max(0, sel - 1)))}>
                      <Icon name="trash" />
                    </button>
                  </div>
                </div>
                <div class="fields">
                  <label class="field">
                    Name
                    <input value={cur.name} onChange={(e) => edit({ name: (e.target as HTMLInputElement).value })} />
                  </label>
                  <label class="field">
                    Kind
                    <input class="mono" value={cur.kind} disabled />
                  </label>
                  <label class="field wide">
                    Base URL
                    <input class="mono" value={cur.baseUrl} onChange={(e) => edit({ baseUrl: (e.target as HTMLInputElement).value })} />
                  </label>
                  <label class="field wide">
                    Model
                    <input class="mono" value={cur.model} placeholder="from the provider's model list" onChange={(e) => edit({ model: (e.target as HTMLInputElement).value.trim() })} />
                  </label>
                  <div class="field wide">
                    API key {needsKey ? <span class="req">needed</span> : <span class="muted">not needed</span>}
                    <span class="inline">
                      <input
                        type="password"
                        autocomplete="off"
                        spellcheck={false}
                        aria-label="API key"
                        placeholder={cur.key ? masked(cur.key) : 'paste a key; it stays in memory'}
                        value={draftKey}
                        onInput={(e) => setDraftKey((e.target as HTMLInputElement).value)}
                      />
                      <button
                        type="button"
                        class="btn small"
                        disabled={!draftKey.trim()}
                        onClick={() => {
                          edit({ key: draftKey.trim() })
                          setDraftKey('')
                        }}
                      >
                        Keep for this session
                      </button>
                      {cur.key ? (
                        <button type="button" class="btn small ghost" onClick={() => edit({ key: undefined } as Partial<ProviderEntry>)}>
                          Forget
                        </button>
                      ) : null}
                      {cur.key ? <Chip tone="ok">{masked(cur.key)}</Chip> : null}
                    </span>
                    <span class="muted small">Held in memory only, never saved or sent. Closing or reloading forgets it. This release calls no provider.</span>
                  </div>
                  <label class="field">
                    Monthly spending cap
                    <NumberInput label="Monthly cap in US dollars" value={cur.monthlyCapUsd} min={0} max={10000} onChange={(v) => edit({ monthlyCapUsd: v })} unit="USD" />
                  </label>
                </div>
              </>
            ) : (
              <div class="pane-empty">Choose a provider, or add one.</div>
            )}
          </div>
        </div>
      </section>
      <SettingsGroup title="With a real zeusd" note="zeus auth stores keys in your system keyring and signs in with OAuth where a provider offers it. Signing in alone does not enable paid requests; spend is reserved against a cap before every call.">
        <div class="row stack">
          <CodeCopy text="zeus auth" label="Copy" />
        </div>
      </SettingsGroup>
      <Provenance>The ladder, its order and the caps are shown as zeusd's provider ladder (spec 09) organises them; in this release nothing is called.</Provenance>
    </>
  )
}
