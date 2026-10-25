// Shared components. States are always printed as the exact word (`awaiting_approval`), with colour as a second
// channel. Settings rows, cards, dialogs and controls follow T3 Code's conventions (DESIGN.md).

import type { ComponentChildren } from 'preact'
import { useEffect, useLayoutEffect, useRef, useState } from 'preact/hooks'

import type { JobState, Segment } from '../model/types.ts'
import type { IconName } from './icons'
import { Icon } from './icons'

export function StateLabel({ state, title }: { state: JobState | string; title?: string }) {
  return (
    <span class={`state s-${state}`} title={title}>
      {state}
    </span>
  )
}

export function Notice({ kind = 'info', children }: { kind?: 'info' | 'warn' | 'error' | 'ok'; children: ComponentChildren }) {
  return (
    <div class={`notice ${kind === 'info' ? '' : kind}`} role={kind === 'error' ? 'alert' : undefined}>
      {children}
    </div>
  )
}

export function Empty({ children }: { children: ComponentChildren }) {
  return <div class="empty">{children}</div>
}

/** Text that came from outside Zeus: a requester's words, a page title, a file name from a server. */
export function Untrusted({ label, text }: { label: string; text: string | undefined }) {
  if (!text) return null
  return (
    <div class="untrusted">
      <span class="label">{label} · not verified by Zeus</span>
      <div class="body">{text}</div>
    </div>
  )
}

export function PageHead({ title, sub, children }: { title: ComponentChildren; sub?: ComponentChildren; children?: ComponentChildren }) {
  return (
    <div class="page-head">
      <div class="titles">
        <h1>{title}</h1>
        {sub ? <p class="sub">{sub}</p> : null}
      </div>
      {children ? <div class="actions">{children}</div> : null}
    </div>
  )
}

export function Kv({ rows }: { rows: [string, ComponentChildren][] }) {
  return (
    <dl class="kv">
      {rows.map(([k, v]) => (
        <>
          <dt>{k}</dt>
          <dd>{v}</dd>
        </>
      ))}
    </dl>
  )
}

export function Tabs<T extends string>({ tabs, value, onChange }: { tabs: [T, string][]; value: T; onChange: (t: T) => void }) {
  return (
    <div class="tabs" role="tablist">
      {tabs.map(([id, label]) => (
        <button type="button" role="tab" aria-selected={value === id} onClick={() => onChange(id)}>
          {label}
        </button>
      ))}
    </div>
  )
}

/** A modal dialog. Escape and the backdrop close it; focus moves into it and returns afterwards. */
export function Dialog({ title, onClose, children, wide, plain }: { title: string; onClose: () => void; children: ComponentChildren; wide?: boolean; plain?: boolean }) {
  const ref = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const prev = document.activeElement as HTMLElement | null
    if (!ref.current?.contains(document.activeElement)) ref.current?.focus()
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      }
    }
    window.addEventListener('keydown', onKey)
    return () => {
      window.removeEventListener('keydown', onKey)
      prev?.focus()
    }
  }, [onClose])
  return (
    <div
      class="dialog-back"
      onMouseDown={(e) => {
        if (e.target === e.currentTarget) onClose()
      }}
    >
      <div class={`dialog ${wide ? 'wide' : ''} ${plain ? 'plain' : ''}`} role="dialog" aria-modal="true" aria-label={title} tabIndex={-1} ref={ref}>
        <div class="dialog-head">
          <h2>{title}</h2>
          <button type="button" class="icon-btn small" aria-label="Close" onClick={onClose}>
            <Icon name="x" />
          </button>
        </div>
        {children}
      </div>
    </div>
  )
}

export function JsonBlock({ value, max = 20_000 }: { value: unknown; max?: number }) {
  let text = JSON.stringify(value, null, 2) ?? 'null'
  if (text.length > max) text = text.slice(0, max) + '\n…'
  return <pre class="code-block mono">{text}</pre>
}

/** A toggle: a button with `role="switch"`, so it works with the keyboard and screen readers. */
export function Switch({ checked, onChange, label, disabled }: { checked: boolean; onChange: (v: boolean) => void; label: string; disabled?: boolean }) {
  return (
    <button type="button" role="switch" aria-checked={checked} aria-label={label} disabled={disabled} class="switch" onClick={() => onChange(!checked)}>
      <span class="knob" />
    </button>
  )
}

export function SettingsGroup({ title, children, note, actions }: { title?: ComponentChildren; children: ComponentChildren; note?: ComponentChildren; actions?: ComponentChildren }) {
  return (
    <section class="sgroup">
      {title || actions ? (
        <div class="sgroup-head">
          {title ? <h2 class="sgroup-title">{title}</h2> : <span />}
          {actions ? <div class="sgroup-actions">{actions}</div> : null}
        </div>
      ) : null}
      <div class="rows">{children}</div>
      {note ? <p class="sgroup-note">{note}</p> : null}
    </section>
  )
}

/** One row: a title and a description at the left, the control at the right. `zeusd` names the daemon setting it maps to. */
export function SettingRow({ title, desc, children, stack, zeusd, planned, fixed }: { title: ComponentChildren; desc?: ComponentChildren; children?: ComponentChildren; stack?: boolean; zeusd?: string; planned?: boolean; fixed?: boolean }) {
  return (
    <div class={`row ${stack ? 'stack' : ''}`}>
      <div class="text">
        <div class="title">
          {title}
          {fixed ? <Chip tone="info">fixed</Chip> : null}
          {planned ? <Chip>planned</Chip> : null}
        </div>
        {desc || zeusd ? (
          <div class="desc">
            {desc}
            {zeusd ? (
              <>
                {desc ? ' ' : ''}
                <span class="zkey mono" title="The zeusd configuration key">
                  zeusd: {zeusd}
                </span>
              </>
            ) : null}
          </div>
        ) : null}
      </div>
      {children ? <div class="control">{children}</div> : null}
    </div>
  )
}

export function Chip({ children, tone = '', title }: { children: ComponentChildren; tone?: '' | 'ok' | 'warn' | 'bad' | 'info' | 'accent'; title?: string }) {
  return (
    <span class={`chip ${tone}`} title={title}>
      {children}
    </span>
  )
}

export function CopyButton({ text, label = 'Copy' }: { text: string; label?: string }) {
  const [done, setDone] = useState(false)
  const copy = async () => {
    try {
      await navigator.clipboard.writeText(text)
    } catch {
      // The text stays selectable beside the button.
    }
    setDone(true)
    setTimeout(() => setDone(false), 2000)
  }
  return (
    <button type="button" class="btn small ghost" onClick={copy} aria-label={`${label} to the clipboard`}>
      <Icon name="copy" /> {done ? 'Copied' : label}
    </button>
  )
}

export function CodeCopy({ text, label }: { text: string; label?: string }) {
  return (
    <div class="codecopy">
      <pre class="mono">{text}</pre>
      <CopyButton text={text} label={label ?? 'Copy'} />
    </div>
  )
}

export function Segmented<T extends string>({ value, options, onChange, label }: { value: T; options: [T, string][]; onChange: (v: T) => void; label: string }) {
  return (
    <div class="segmented" role="radiogroup" aria-label={label}>
      {options.map(([id, text]) => (
        <button type="button" role="radio" aria-checked={value === id} class={value === id ? 'on' : ''} onClick={() => onChange(id)}>
          {text}
        </button>
      ))}
    </div>
  )
}

export function Slider({ value, min, max, step = 1, onChange, label, unit = '' }: { value: number; min: number; max: number; step?: number; onChange: (v: number) => void; label: string; unit?: string }) {
  return (
    <div class="slider">
      <input type="range" min={min} max={max} step={step} value={value} aria-label={label} onInput={(e) => onChange(Number((e.target as HTMLInputElement).value))} />
      <output class="mono">
        {value}
        {unit}
      </output>
    </div>
  )
}

export function NumberInput({ value, min, max, onChange, label, unit, step = 1 }: { value: number; min: number; max: number; onChange: (v: number) => void; label: string; unit?: string; step?: number }) {
  return (
    <span class="withunit">
      <input
        type="number"
        min={min}
        max={max}
        step={step}
        value={value}
        aria-label={label}
        onChange={(e) => {
          const v = Number((e.target as HTMLInputElement).value)
          if (Number.isFinite(v)) onChange(Math.min(max, Math.max(min, v)))
        }}
      />
      {unit ? <span class="muted">{unit}</span> : null}
    </span>
  )
}

export function Select<T extends string>({ value, options, onChange, label }: { value: T; options: [T, string][]; onChange: (v: T) => void; label: string }) {
  return (
    <select aria-label={label} value={value} onChange={(e) => onChange((e.target as HTMLSelectElement).value as T)}>
      {options.map(([id, text]) => (
        <option value={id}>{text}</option>
      ))}
    </select>
  )
}

/** A progress bar; `null` draws an indeterminate stripe for an unknown size. */
export function Progress({ value, tone = '' }: { value: number | null; tone?: '' | 'ok' | 'bad' | 'warn' | 'muted' }) {
  return (
    <span class={`progress ${tone} ${value === null ? 'unknown' : ''}`} role="progressbar" aria-valuemin={0} aria-valuemax={100} aria-valuenow={value ?? undefined}>
      <span style={{ width: `${value === null ? 100 : Math.max(0, Math.min(100, value)).toFixed(1)}%` }} />
    </span>
  )
}

/** Per-segment progress: one cell per byte range, filled by its committed share. */
export function SegmentBar({ segments, size }: { segments: Segment[]; size: number | null }) {
  if (!segments.length || !size) return <span class="muted">No byte ranges yet.</span>
  return (
    <div class="segbar" role="img" aria-label={`${segments.length} byte ranges`}>
      {segments.map((s) => {
        const span = s.end - s.start
        const pct = span ? (s.done / span) * 100 : 0
        return (
          <span class={`seg ${s.state}`} style={{ flexGrow: String(span) }} title={`${s.state}: ${pct.toFixed(0)} %`}>
            <i style={{ width: `${pct.toFixed(1)}%` }} />
          </span>
        )
      })}
    </div>
  )
}

export interface MenuItem {
  label: string
  icon?: IconName
  hint?: string
  disabled?: boolean | string
  danger?: boolean
  run?: () => void
  sep?: boolean
  sub?: MenuItem[]
}

/** A context menu at a point, kept inside the window. Arrow keys move, Enter runs, Escape closes. */
export function ContextMenu({ x, y, items, onClose, label }: { x: number; y: number; items: MenuItem[]; onClose: () => void; label: string }) {
  const ref = useRef<HTMLDivElement>(null)
  const [pos, setPos] = useState({ x, y })
  const [sub, setSub] = useState<number | null>(null)
  useLayoutEffect(() => {
    const el = ref.current
    if (!el) return
    const r = el.getBoundingClientRect()
    setPos({ x: Math.max(4, Math.min(x, window.innerWidth - r.width - 4)), y: Math.max(4, Math.min(y, window.innerHeight - r.height - 4)) })
    ;(el.querySelector('button:not(:disabled)') as HTMLButtonElement | null)?.focus()
  }, [x, y])
  useEffect(() => {
    const onDown = (e: MouseEvent) => {
      if (ref.current && !ref.current.contains(e.target as Node)) onClose()
    }
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') {
        e.stopPropagation()
        onClose()
      }
    }
    window.addEventListener('mousedown', onDown, true)
    window.addEventListener('keydown', onKey, true)
    window.addEventListener('blur', onClose)
    return () => {
      window.removeEventListener('mousedown', onDown, true)
      window.removeEventListener('keydown', onKey, true)
      window.removeEventListener('blur', onClose)
    }
  }, [onClose])
  const onKeyDown = (e: KeyboardEvent) => {
    const buttons = [...(ref.current?.querySelectorAll(':scope > .ctxitem > button:not(:disabled)') ?? [])] as HTMLButtonElement[]
    const i = buttons.indexOf(document.activeElement as HTMLButtonElement)
    if (e.key === 'ArrowDown') {
      e.preventDefault()
      buttons[(i + 1) % buttons.length]?.focus()
    } else if (e.key === 'ArrowUp') {
      e.preventDefault()
      buttons[(i - 1 + buttons.length) % buttons.length]?.focus()
    }
  }
  return (
    <div class="ctxmenu" role="menu" aria-label={label} ref={ref} style={{ left: `${pos.x}px`, top: `${pos.y}px` }} onKeyDown={onKeyDown}>
      {items.map((it, i) =>
        it.sep ? (
          <div class="ctxsep" role="separator" />
        ) : (
          <div class="ctxitem" onMouseEnter={() => setSub(it.sub ? i : null)}>
            <button
              type="button"
              role="menuitem"
              class={`${it.danger ? 'danger' : ''} ${sub === i ? 'open' : ''}`}
              disabled={!!it.disabled}
              aria-haspopup={it.sub ? 'menu' : undefined}
              title={typeof it.disabled === 'string' ? it.disabled : undefined}
              onClick={() => {
                if (it.sub) {
                  setSub(sub === i ? null : i)
                  return
                }
                it.run?.()
                onClose()
              }}
            >
              {it.icon ? <Icon name={it.icon} /> : <span class="ico-sp" />}
              <span class="name">{it.label}</span>
              {it.hint ? <span class="hint">{it.hint}</span> : null}
              {it.sub ? <Icon name="chevronright" /> : null}
            </button>
            {it.sub && sub === i ? (
              <div class="ctxmenu sub" role="menu">
                {it.sub.map((s) => (
                  <div class="ctxitem">
                    <button
                      type="button"
                      role="menuitem"
                      disabled={!!s.disabled}
                      onClick={() => {
                        s.run?.()
                        onClose()
                      }}
                    >
                      {s.icon ? <Icon name={s.icon} /> : <span class="ico-sp" />}
                      <span class="name">{s.label}</span>
                    </button>
                  </div>
                ))}
              </div>
            ) : null}
          </div>
        ),
      )}
    </div>
  )
}

/** A status line under a page title: what in this page is real in zeusd today and what is planned. */
export function Provenance({ children }: { children: ComponentChildren }) {
  return (
    <p class="provenance">
      <Icon name="info" />
      <span>{children}</span>
    </p>
  )
}
