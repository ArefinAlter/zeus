// Device preferences: how the client looks and behaves on this machine. Kept in the webview's own storage in the
// desktop app, never sent anywhere and never holding a secret. The copy on the Zeus website is built with
// VITE_ZEUS_NO_STORAGE=1 and keeps them in memory for the visit only, because that site promises no cookies and no
// local storage. Storage may be unavailable (private windows, blocked site data): every access is guarded.

import { useEffect, useState } from 'preact/hooks'

export type Appearance = 'system' | 'light' | 'dark'
export type Density = 'comfortable' | 'compact'
export type ContentWidth = 'comfortable' | 'wide' | 'full'
export type StatusColors = 'standard' | 'colorblind'
export type Notifications = 'all' | 'important' | 'off'

export interface ColumnPref {
  id: string
  width: number
  visible: boolean
}

export interface Prefs {
  appearance: Appearance
  theme: string
  /** 50 to 150, 100 = as designed. Scales the strength of borders and rules. */
  contrast: number
  density: Density
  contentWidth: ContentWidth
  statusColors: StatusColors
  reduceMotion: boolean
  startPage: string
  notifications: Notifications
  /** Short sounds on completion and failure (drawn with the Web Audio API; nothing is fetched). */
  sounds: boolean
  confirmRemove: boolean
  /** Show the speed and active count in the window title. */
  titleSpeed: boolean
  statusBar: boolean
  sidebarOpen: boolean
  sidebarCategories: boolean
  agentDetails: boolean
  /** Downloads list: column order, widths and visibility. */
  columns: ColumnPref[]
  sortBy: string
  sortDir: 'asc' | 'desc'
  /** Files: the folders and preview panes. */
  filesFolders: boolean
  filesPreview: boolean
  /** Command id -> combo, only for commands the person rebound (`''` = unbound). */
  keybindings: Record<string, string>
  keyWhen: Record<string, string>
  /** How solid menus, dialogs and the palette are, 50 to 100 percent. */
  glass: number
  customThemes: CustomTheme[]
}

export interface CustomTheme {
  id: string
  label: string
  /** 0 to 360. */
  hue: number
  /** 0 to 100: how strongly surfaces carry the hue. */
  tint: number
  /** `#rrggbb` for the accent in the light scheme; the dark scheme derives from it. */
  accent: string
}

export const COLUMN_IDS = ['name', 'size', 'progress', 'speed', 'eta', 'status', 'added', 'category', 'queue', 'host'] as const

export const DEFAULT_COLUMNS: ColumnPref[] = [
  { id: 'name', width: 290, visible: true },
  { id: 'size', width: 90, visible: true },
  { id: 'progress', width: 160, visible: true },
  { id: 'speed', width: 96, visible: true },
  { id: 'eta', width: 92, visible: true },
  { id: 'status', width: 210, visible: true },
  { id: 'added', width: 120, visible: true },
  { id: 'category', width: 100, visible: true },
  { id: 'queue', width: 140, visible: true },
  { id: 'host', width: 170, visible: true },
]

export const DEFAULTS: Prefs = {
  appearance: 'system',
  theme: 'zeus',
  contrast: 100,
  density: 'comfortable',
  contentWidth: 'comfortable',
  statusColors: 'standard',
  reduceMotion: false,
  startPage: 'downloads',
  notifications: 'important',
  sounds: false,
  confirmRemove: true,
  titleSpeed: true,
  statusBar: true,
  sidebarOpen: true,
  sidebarCategories: true,
  agentDetails: false,
  columns: DEFAULT_COLUMNS,
  sortBy: 'added',
  sortDir: 'desc',
  filesFolders: true,
  filesPreview: true,
  keybindings: {},
  keyWhen: {},
  glass: 92,
  customThemes: [],
}

const KEY = 'zeus-client:prefs:v1'

const PERSIST = import.meta.env.VITE_ZEUS_NO_STORAGE !== '1'

/** Whether preferences outlive the visit (false on the website copy). */
export const PREFS_PERSIST = PERSIST

function read(): Prefs {
  if (!PERSIST) return { ...DEFAULTS }
  try {
    const raw = window.localStorage.getItem(KEY)
    if (!raw) return { ...DEFAULTS }
    return sanitize(JSON.parse(raw) as Partial<Prefs>)
  } catch {
    return { ...DEFAULTS }
  }
}

function pick<T extends string>(v: unknown, allowed: readonly T[], fallback: T): T {
  return typeof v === 'string' && (allowed as readonly string[]).includes(v) ? (v as T) : fallback
}

function bool(v: unknown, fallback: boolean): boolean {
  return typeof v === 'boolean' ? v : fallback
}

function columns(v: unknown): ColumnPref[] {
  if (!Array.isArray(v)) return DEFAULT_COLUMNS
  const out: ColumnPref[] = []
  for (const c of v) {
    if (!c || typeof c !== 'object') continue
    const x = c as Partial<ColumnPref>
    if (typeof x.id !== 'string' || !(COLUMN_IDS as readonly string[]).includes(x.id) || out.some((o) => o.id === x.id)) continue
    out.push({ id: x.id, width: typeof x.width === 'number' && Number.isFinite(x.width) ? Math.min(800, Math.max(56, Math.round(x.width))) : 120, visible: x.id === 'name' ? true : bool(x.visible, true) })
  }
  for (const d of DEFAULT_COLUMNS) if (!out.some((o) => o.id === d.id)) out.push(d)
  return out
}

/** Whatever was stored, as a complete and valid `Prefs`: unknown members and wrong types fall back to the defaults. */
export function sanitize(p: Partial<Prefs>): Prefs {
  const kb: Record<string, string> = {}
  if (p.keybindings && typeof p.keybindings === 'object')
    for (const [k, v] of Object.entries(p.keybindings)) if (typeof v === 'string' && v.length <= 40 && /^[a-z0-9_.-]+$/.test(k)) kb[k] = v
  const kw: Record<string, string> = {}
  if (p.keyWhen && typeof p.keyWhen === 'object')
    for (const [k, v] of Object.entries(p.keyWhen)) if (typeof v === 'string' && ['always', '!inputFocus', '!modalOpen'].includes(v) && /^[a-z0-9_.-]+$/.test(k)) kw[k] = v
  const custom: CustomTheme[] = []
  if (Array.isArray(p.customThemes))
    for (const t of p.customThemes.slice(0, 12)) {
      if (!t || typeof t !== 'object') continue
      const c = t as Partial<CustomTheme>
      if (typeof c.id === 'string' && /^[a-z0-9-]{1,32}$/.test(c.id) && typeof c.label === 'string' && c.label.trim() && typeof c.hue === 'number' && typeof c.tint === 'number' && typeof c.accent === 'string' && /^#[0-9a-fA-F]{6}$/.test(c.accent))
        custom.push({ id: c.id, label: c.label.trim().slice(0, 32), hue: Math.min(360, Math.max(0, Math.round(c.hue))), tint: Math.min(100, Math.max(0, Math.round(c.tint))), accent: c.accent.toLowerCase() })
    }
  const num = (v: unknown, lo: number, hi: number, d: number) => (typeof v === 'number' && Number.isFinite(v) ? Math.min(hi, Math.max(lo, Math.round(v))) : d)
  return {
    appearance: pick(p.appearance, ['system', 'light', 'dark'], DEFAULTS.appearance),
    theme: typeof p.theme === 'string' && /^[a-z0-9-]{1,32}$/.test(p.theme) ? p.theme : DEFAULTS.theme,
    contrast: num(p.contrast, 50, 150, DEFAULTS.contrast),
    density: pick(p.density, ['comfortable', 'compact'], DEFAULTS.density),
    contentWidth: pick(p.contentWidth, ['comfortable', 'wide', 'full'], DEFAULTS.contentWidth),
    statusColors: pick(p.statusColors, ['standard', 'colorblind'], DEFAULTS.statusColors),
    reduceMotion: bool(p.reduceMotion, DEFAULTS.reduceMotion),
    startPage: typeof p.startPage === 'string' && /^[a-z/-]{0,40}$/.test(p.startPage) ? p.startPage : DEFAULTS.startPage,
    notifications: pick(p.notifications, ['all', 'important', 'off'], DEFAULTS.notifications),
    sounds: bool(p.sounds, DEFAULTS.sounds),
    confirmRemove: bool(p.confirmRemove, DEFAULTS.confirmRemove),
    titleSpeed: bool(p.titleSpeed, DEFAULTS.titleSpeed),
    statusBar: bool(p.statusBar, DEFAULTS.statusBar),
    sidebarOpen: bool(p.sidebarOpen, DEFAULTS.sidebarOpen),
    sidebarCategories: bool(p.sidebarCategories, DEFAULTS.sidebarCategories),
    agentDetails: bool(p.agentDetails, DEFAULTS.agentDetails),
    columns: columns(p.columns),
    sortBy: typeof p.sortBy === 'string' && (COLUMN_IDS as readonly string[]).includes(p.sortBy) ? p.sortBy : DEFAULTS.sortBy,
    sortDir: pick(p.sortDir, ['asc', 'desc'], DEFAULTS.sortDir),
    filesFolders: bool(p.filesFolders, DEFAULTS.filesFolders),
    filesPreview: bool(p.filesPreview, DEFAULTS.filesPreview),
    keybindings: kb,
    keyWhen: kw,
    glass: num(p.glass, 50, 100, DEFAULTS.glass),
    customThemes: custom,
  }
}

let current: Prefs = read()
const listeners = new Set<(p: Prefs) => void>()

export function getPrefs(): Prefs {
  return current
}

export function setPrefs(patch: Partial<Prefs>): Prefs {
  current = sanitize({ ...current, ...patch })
  try {
    if (PERSIST) window.localStorage.setItem(KEY, JSON.stringify(current))
  } catch {
    // The preference holds for this visit only.
  }
  for (const l of listeners) l(current)
  for (const w of watchers) w(current, false)
  return current
}

/** Apply preferences kept by the pre-alpha service for this sandbox (hosted build), without writing them back. */
export function hydratePrefs(stored: unknown): void {
  if (!stored || typeof stored !== 'object') return
  current = sanitize({ ...current, ...(stored as Partial<Prefs>) })
  for (const l of listeners) l(current)
  for (const w of watchers) w(current, true)
}

const watchers = new Set<(p: Prefs, hydrated: boolean) => void>()

/** Be told when a person changes a preference (not when stored ones are applied). */
export function subscribePrefs(f: (p: Prefs) => void): () => void {
  const w = (p: Prefs, hydrated: boolean) => {
    if (!hydrated) f(p)
  }
  watchers.add(w)
  return () => {
    watchers.delete(w)
  }
}

export function resetPrefs(): Prefs {
  current = { ...DEFAULTS }
  try {
    if (PERSIST) window.localStorage.removeItem(KEY)
  } catch {
    // Nothing stored, nothing to remove.
  }
  for (const l of listeners) l(current)
  return current
}

/** The preferences, re-rendering the component when any of them changes. */
export function usePrefs(): Prefs {
  const [p, setP] = useState(current)
  useEffect(() => {
    listeners.add(setP)
    setP(current)
    return () => {
      listeners.delete(setP)
    }
  }, [])
  return p
}
