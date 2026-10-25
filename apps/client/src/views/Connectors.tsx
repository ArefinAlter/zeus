// Connectors: cloud destinations and web sources, with an honest status per Zeus's implementation ledger. "Built" is a
// native adapter, "Via rclone" works through an rclone remote you configure, "Manifest only" is described and not
// built. Credentials are configured at a terminal into the OS keyring; no screen here has a field for a secret.

import { useMemo, useState } from 'preact/hooks'

import type { Connector, ConnectorStatus } from '../backend/connectors'
import { CONNECTORS, STATUS_LABEL } from '../backend/connectors'
import { toggleConnector, useStore } from '../backend/store'
import { href } from '../lib/router'
import { Chip, CodeCopy, Empty, Notice, PageHead, Provenance } from '../ui/components'
import type { IconName } from '../ui/icons'
import { Icon } from '../ui/icons'

const GROUP_ICON: Record<Connector['group'], IconName> = {
  'Cloud storage': 'cloud',
  'Object storage': 'database',
  'File transfer': 'server',
  'Web sources': 'globe',
  'Messaging and notes': 'chat',
  'Peer to peer': 'layers',
}

function tone(s: ConnectorStatus): 'ok' | 'info' | '' {
  return s === 'built' ? 'ok' : s === 'rclone' ? 'info' : ''
}

export function Connectors({ id }: { id?: string }) {
  const added = useStore((s) => s.connectorsAdded)
  const [q, setQ] = useState('')
  const [status, setStatus] = useState<'all' | ConnectorStatus>('all')
  const [group, setGroup] = useState<'all' | Connector['group']>('all')
  const groups = [...new Set(CONNECTORS.map((c) => c.group))]
  const shown = useMemo(() => {
    const n = q.trim().toLowerCase()
    return CONNECTORS.filter((c) => (status === 'all' || c.status === status) && (group === 'all' || c.group === group) && (!n || `${c.name} ${c.summary} ${c.hosts.join(' ')} ${c.group}`.toLowerCase().includes(n)))
  }, [q, status, group])
  const c = id ? CONNECTORS.find((x) => x.id === id) : undefined
  if (c) return <ConnectorDetail c={c} added={added.includes(c.id)} />
  return (
    <div class="page">
      <PageHead title="Connectors" sub="Where Zeus can deliver finished files, and where it can read from. Credentials are set at a terminal into your system's keyring." />
      <div class="kbbar">
        <label class="searchbox">
          <Icon name="search" />
          <input type="search" placeholder="Search connectors" aria-label="Search connectors" value={q} onInput={(e) => setQ((e.target as HTMLInputElement).value)} />
        </label>
      </div>
      <div class="chips">
        {(['all', 'built', 'rclone', 'manifest'] as const).map((s) => (
          <button type="button" class={`catchip ${status === s ? 'on' : ''}`} aria-pressed={status === s} onClick={() => setStatus(s)}>
            {s === 'all' ? 'Any status' : STATUS_LABEL[s]} <span class="n">{s === 'all' ? CONNECTORS.length : CONNECTORS.filter((x) => x.status === s).length}</span>
          </button>
        ))}
        <span class="vsep" />
        {(['all', ...groups] as const).map((g) => (
          <button type="button" class={`catchip ${group === g ? 'on' : ''}`} aria-pressed={group === g} onClick={() => setGroup(g)}>
            {g === 'all' ? 'Every kind' : g}
          </button>
        ))}
      </div>
      {shown.length ? (
        <div class="cat-grid">
          {shown.map((x) => (
            <a class="ccard" href={href('connectors', x.id)}>
              <div class="ccard-head">
                <span class="well">
                  <Icon name={GROUP_ICON[x.group]} />
                </span>
                <span class="ccard-title">
                  <span class="name">{x.name}</span>
                  <span class="vendor">
                    {x.group} · {x.role}
                  </span>
                </span>
              </div>
              <p class="ccard-summary">{x.summary}</p>
              <div class="ccard-foot">
                <Chip tone={tone(x.status)}>{STATUS_LABEL[x.status]}</Chip>
                {added.includes(x.id) ? <Chip tone="accent">added</Chip> : null}
                <span class="muted small">{x.auth}</span>
              </div>
            </a>
          ))}
        </div>
      ) : (
        <Empty>No connector matches.</Empty>
      )}
      <Provenance>Statuses follow Zeus's implementation ledger and spec 14 on cloud delivery. Live-account acceptance of the built adapters is still pending there.</Provenance>
    </div>
  )
}

function ConnectorDetail({ c, added }: { c: Connector; added: boolean }) {
  return (
    <div class="page">
      <PageHead title={c.name} sub={c.summary}>
        <a class="btn" href={href('connectors')}>
          All connectors
        </a>
      </PageHead>
      <div class="panel">
        <dl class="kv">
          <dt>status</dt>
          <dd>
            <Chip tone={tone(c.status)}>{STATUS_LABEL[c.status]}</Chip>
          </dd>
          <dt>role</dt>
          <dd>{c.role === 'destination' ? 'Delivery destination' : c.role === 'source' ? 'Source' : 'Notifications'}</dd>
          <dt>sign-in</dt>
          <dd>{c.auth}</dd>
          <dt>hosts</dt>
          <dd class="mono">{c.hosts.join(', ')}</dd>
          {c.modes ? (
            <>
              <dt>modes</dt>
              <dd>{c.modes.join(' · ')}</dd>
            </>
          ) : null}
          <dt>today</dt>
          <dd>{c.note}</dd>
        </dl>
      </div>
      {c.status === 'manifest' ? (
        <Notice>This connector is described, not built. Nothing here can turn it on.</Notice>
      ) : (
        <>
          <h2>Set it up</h2>
          <p class="muted">
            {c.status === 'built' ? 'Sign in at a terminal; the token goes to your system keyring, never to this app.' : 'Create the remote with rclone first, then register it with Zeus at a terminal. Credentials stay in rclone.conf.'} Configuring an account needs a fresh review at a physical terminal.
          </p>
          <CodeCopy text={c.status === 'built' ? 'zeus cloud accounts configure' : `rclone config\nzeus cloud accounts configure`} />
          <div class="toolbar">
            <button type="button" class={`btn ${added ? '' : 'primary'}`} onClick={() => toggleConnector(c.id, !added)}>
              {added ? 'Remove from this list' : 'Add to my destinations'}
            </button>
            <span class="muted small">In this release the list is sample data; no account is contacted.</span>
          </div>
        </>
      )}
    </div>
  )
}
