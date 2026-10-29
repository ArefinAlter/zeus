// The composer's model picker for the desktop app: the provider ladder with what each entry needs before it can answer.

import { providerBlocker } from '../../backend/agentNative'
import { useAgentConfig } from '../../backend/useAgentConfig'
import { href } from '../../lib/router'
import { Chip } from '../../ui/components'
import { Icon } from '../../ui/icons'
import { Popover } from '../../ui/Popover'

export function ModelPicker({ providerId, onPick }: { providerId?: string | undefined; onPick: (providerId: string) => void }) {
  const cfg = useAgentConfig()
  const list = cfg?.providers.filter((p) => p.enabled) ?? []
  const cur = (providerId ? cfg?.providers.find((p) => p.id === providerId) : undefined) ?? (cfg ? (list.find((p) => !providerBlocker(cfg, p)) ?? list[0]) : undefined)
  const blocker = cfg && cur ? providerBlocker(cfg, cur) : null
  const isSub = (k: string) => ['codex', 'claude', 'cursor', 'antigravity', 'grok'].includes(k)
  const label = cur ? `${cur.name}${isSub(cur.kind) ? ' · Subscription' : cur.model ? ` · ${cur.model}` : ''}` : 'No model yet'
  return (
    <Popover label={`Model: ${label}`} title="Which model answers in this thread" trigger={<><Icon name="sparkle" /> <span class="cbl">{label}</span> <Icon name="chevron" /></>}>
      {(close) => (
        <div class="model-pick">
          <div class="pophead">Select AI Provider & Model</div>
          {list.length ? (
            list.map((p) => {
              const b = cfg ? providerBlocker(cfg, p) : null
              const sub = isSub(p.kind)
              return (
                <button type="button" role="menuitemradio" aria-checked={p.id === cur?.id} class={p.id === cur?.id ? 'on' : ''} onClick={() => (onPick(p.id), close())}>
                  <span class="name">
                    {p.name} <span class="mono small">{sub ? '(Active Subscription)' : (p.model || 'no model yet')}</span>
                  </span>
                  <span class="desc">
                    {sub ? <Chip tone="ok">Pro/Team</Chip> : b ? <Chip tone="warn">{b.text}</Chip> : <Chip tone="ok">ready</Chip>} {p.local ? <Chip>local</Chip> : null}
                  </span>
                </button>
              )
            })
          ) : (
            <div class="popfoot">No provider is enabled. Choose an installed agent or add a key in Settings, Providers.</div>
          )}
          {blocker && cur ? <div class="popfoot">{cur.name}: {blocker.text}.</div> : null}
          <a class="popfoot" href={href('settings', 'providers')} onClick={close}>
            Manage Providers & Subscriptions →
          </a>
        </div>
      )}
    </Popover>
  )
}
