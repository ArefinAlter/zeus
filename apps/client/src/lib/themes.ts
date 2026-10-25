// Named themes and how preferences become CSS. A theme is a set of token overrides for the light and the dark
// scheme (the base tokens, in tokens.css, are the default `zeus` theme: neutral zinc surfaces and the ultramarine
// accent of the Zeus brand). The same data draws the swatches in Settings, so a preview can never disagree with the result.
//
// The tinted themes are generated from one hue, the way T3 Code's built-in themes are built from a hue per
// theme: surfaces carry a trace of the hue, the primary role carries the rest.

import type { CustomTheme, Prefs } from './prefs'

export type Scheme = 'light' | 'dark'

export interface Theme {
  id: string
  label: string
  /** One line for the picker. */
  note: string
  light: Record<string, string>
  dark: Record<string, string>
}

function tinted(id: string, label: string, note: string, h: number, strength = 1, accent?: { light: string; dark: string }): Theme {
  const c = (v: number) => (v * strength).toFixed(4)
  return {
    id,
    label,
    note,
    light: {
      '--background': `oklch(0.975 ${c(0.006)} ${h})`,
      '--foreground': `oklch(0.24 ${c(0.03)} ${h})`,
      '--card': `oklch(0.996 ${c(0.003)} ${h})`,
      '--popover': `oklch(0.996 ${c(0.003)} ${h})`,
      '--muted': `oklch(0.962 ${c(0.01)} ${h})`,
      '--muted-foreground': `oklch(0.5 ${c(0.03)} ${h})`,
      '--accent': `oklch(0.94 ${c(0.014)} ${h})`,
      '--secondary': `oklch(0.962 ${c(0.01)} ${h})`,
      '--border': `oklch(0.9 ${c(0.014)} ${h})`,
      '--input': `oklch(0.84 ${c(0.018)} ${h})`,
      '--sidebar': `oklch(0.95 ${c(0.014)} ${h})`,
      '--sidebar-row-hover': `oklch(0.972 ${c(0.01)} ${h})`,
      '--sidebar-row-selected': `oklch(0.996 ${c(0.003)} ${h})`,
      '--primary': accent?.light ?? `oklch(0.5 0.12 ${h})`,
      '--primary-foreground': '#ffffff',
      '--code-background': `oklch(0.996 ${c(0.003)} ${h})`,
    },
    dark: {
      '--background': `oklch(0.235 ${c(0.02)} ${h})`,
      '--foreground': `oklch(0.97 ${c(0.01)} ${h})`,
      '--card': `oklch(0.265 ${c(0.022)} ${h})`,
      '--popover': `oklch(0.285 ${c(0.024)} ${h})`,
      '--muted': 'rgb(255 255 255 / 0.04)',
      '--muted-foreground': `oklch(0.72 ${c(0.03)} ${h})`,
      '--accent': 'rgb(255 255 255 / 0.06)',
      '--secondary': 'rgb(255 255 255 / 0.04)',
      '--border': 'rgb(255 255 255 / 0.09)',
      '--input': 'rgb(255 255 255 / 0.13)',
      '--sidebar': `oklch(0.215 ${c(0.02)} ${h})`,
      '--sidebar-row-hover': 'rgb(255 255 255 / 0.06)',
      '--sidebar-row-selected': 'rgb(255 255 255 / 0.08)',
      '--primary': accent?.dark ?? `oklch(0.78 0.12 ${h})`,
      '--primary-foreground': `oklch(0.2 ${c(0.03)} ${h})`,
      '--code-background': `oklch(0.2 ${c(0.02)} ${h})`,
    },
  }
}

export const THEMES: Theme[] = [
  { id: 'zeus', label: 'Zeus', note: 'Neutral surfaces with the ultramarine accent.', light: {}, dark: {} },
  {
    id: 'marble',
    label: 'Marble',
    note: 'The brand palette: Parian marble by day, storm by night.',
    light: {
      '--background': '#f7f3ec',
      '--foreground': '#1b2233',
      '--card': '#fbf8f3',
      '--popover': '#fbf8f3',
      '--muted': '#efe9de',
      '--muted-foreground': '#4a5268',
      '--accent': '#e9e2d6',
      '--secondary': '#efe9de',
      '--border': '#e2d9ca',
      '--input': '#cfc3ae',
      '--sidebar': '#efe9de',
      '--sidebar-row-hover': '#f3eee5',
      '--sidebar-row-selected': '#fbf8f3',
      '--primary': '#2347a8',
      '--primary-foreground': '#ffffff',
      '--code-background': '#fbf8f3',
    },
    dark: {
      '--background': '#121a2c',
      '--foreground': '#ece6da',
      '--card': '#172036',
      '--popover': '#1b253d',
      '--muted': 'rgb(255 255 255 / 0.04)',
      '--muted-foreground': '#a7adbd',
      '--accent': 'rgb(255 255 255 / 0.06)',
      '--secondary': 'rgb(255 255 255 / 0.04)',
      '--border': 'rgb(236 230 218 / 0.1)',
      '--input': 'rgb(236 230 218 / 0.16)',
      '--sidebar': '#0f1626',
      '--sidebar-row-hover': 'rgb(255 255 255 / 0.05)',
      '--sidebar-row-selected': 'rgb(255 255 255 / 0.08)',
      '--primary': '#9db4f5',
      '--primary-foreground': '#121a2c',
      '--code-background': '#0c1220',
    },
  },
  tinted('grove', 'Grove', 'Green-tinted surfaces.', 150),
  tinted('ember', 'Ember', 'Warm surfaces with an ember accent.', 45, 0.8, { light: '#9c4419', dark: '#e8662e' }),
  tinted('iris', 'Iris', 'Violet-tinted surfaces.', 294),
  {
    id: 'graphite',
    label: 'Graphite',
    note: 'No colour at all except the status words.',
    light: {
      '--primary': 'oklch(0.3 0 0)',
      '--primary-foreground': '#ffffff',
    },
    dark: {
      '--primary': 'oklch(0.92 0 0)',
      '--primary-foreground': 'oklch(0.18 0 0)',
    },
  },
]

/** A theme the person made, from a hue, a tint strength and an accent. */
export function customTheme(c: CustomTheme): Theme {
  const light = c.accent
  return tinted(c.id, c.label, 'Your theme.', c.hue, c.tint / 100, { light, dark: lighten(light) })
}

/** The dark scheme's accent: the same hue and chroma, lifted to a lightness that reads on dark surfaces. */
function lighten(hex: string): string {
  const n = parseInt(hex.slice(1), 16)
  const r = (n >> 16) & 255
  const g = (n >> 8) & 255
  const b = n & 255
  const mix = (v: number) => Math.round(v + (255 - v) * 0.32)
  return `#${[mix(r), mix(g), mix(b)].map((v) => v.toString(16).padStart(2, '0')).join('')}`
}

export function allThemes(custom: CustomTheme[]): Theme[] {
  return [...THEMES, ...custom.map(customTheme)]
}

export function themeById(id: string, custom: CustomTheme[] = []): Theme {
  return allThemes(custom).find((t) => t.id === id) ?? (THEMES[0] as Theme)
}

/** Swatch colours for the picker: the page background and the primary role, per scheme. */
export function swatch(t: Theme, scheme: Scheme): { background: string; primary: string } {
  const base: Record<Scheme, { background: string; primary: string }> = {
    light: { background: 'oklch(0.992 0 0)', primary: '#2347a8' },
    dark: { background: 'oklch(0.145 0 0)', primary: '#8ea7f0' },
  }
  const o = t[scheme]
  return { background: o['--background'] ?? base[scheme].background, primary: o['--primary'] ?? base[scheme].primary }
}

/** The default theme's surface colours, for the previews (the same values as tokens.css). */
const BASE_PREVIEW: Record<Scheme, Record<string, string>> = {
  light: { '--background': 'oklch(0.992 0 0)', '--sidebar': 'oklch(0.985 0 0)', '--card': '#ffffff', '--border': 'oklch(0.92 0.004 286)', '--muted': 'oklch(0.97 0 0)', '--primary': '#2347a8', '--foreground': 'oklch(0.274 0.006 286)' },
  dark: { '--background': 'oklch(0.145 0 0)', '--sidebar': 'oklch(0.17 0 0)', '--card': 'oklch(0.17 0 0)', '--border': 'rgb(255 255 255 / 0.08)', '--muted': 'rgb(255 255 255 / 0.05)', '--primary': '#8ea7f0', '--foreground': 'oklch(0.97 0 0)' },
}

/** The tokens a preview card draws with: the theme's own, over the default theme's. */
export function previewTokens(t: Theme, scheme: Scheme): Record<string, string> {
  return { ...BASE_PREVIEW[scheme], ...t[scheme] }
}

const OVERRIDDEN = new Set<string>()

export function systemScheme(): Scheme {
  try {
    return window.matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light'
  } catch {
    return 'light'
  }
}

export function effectiveScheme(p: Prefs): Scheme {
  return p.appearance === 'system' ? systemScheme() : p.appearance
}

/** Writes the preferences into the document: scheme, theme tokens, density, width, status colours, contrast. */
export function applyPrefs(p: Prefs): void {
  const root = document.documentElement
  const scheme = effectiveScheme(p)
  root.dataset.scheme = scheme
  root.dataset.themeId = p.theme
  root.dataset.density = p.density
  root.dataset.width = p.contentWidth
  root.dataset.status = p.statusColors
  root.dataset.motion = p.reduceMotion ? 'reduced' : 'normal'
  for (const name of OVERRIDDEN) root.style.removeProperty(name)
  OVERRIDDEN.clear()
  for (const [name, value] of Object.entries(themeById(p.theme, p.customThemes)[scheme])) {
    if (p.statusColors === 'colorblind' && /^--(success|error)/.test(name)) continue
    root.style.setProperty(name, value)
    OVERRIDDEN.add(name)
  }
  // Contrast: below 100 the rules fade, above 100 they mix toward the text colour.
  root.style.setProperty('--glass', `${p.glass}%`)
  root.style.setProperty('--c-low', String(Math.min(p.contrast, 100) / 100))
  root.style.setProperty('--c-high', String(Math.max(p.contrast - 100, 0) * 0.6))
}

/** Calls `f` whenever the operating system's colour scheme changes. */
export function watchSystemScheme(f: () => void): () => void {
  try {
    const mq = window.matchMedia('(prefers-color-scheme: dark)')
    mq.addEventListener('change', f)
    return () => mq.removeEventListener('change', f)
  } catch {
    return () => undefined
  }
}
