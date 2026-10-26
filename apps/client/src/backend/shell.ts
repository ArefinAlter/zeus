// What the desktop app does around its window, through the engine layer: start at sign-in, start minimized, the
// notification-area icon, closing to it, system notifications while the window is hidden, and a manual update check.

import { call } from './remote.ts'

export interface ShellSettings {
  launch_at_login: boolean
  start_minimized: boolean
  show_tray: boolean
  close_to_tray: boolean
  check_updates_weekly: boolean
}

export interface ShellView {
  settings: ShellSettings
  applied_by_shell: boolean
  tray_available: boolean
  version: string
  note?: string | null
}

export interface UpdateInfo {
  current: string
  latest: string
  newer: boolean
  url: string | null
  name: string | null
}

export const shellApi = {
  get: () => call<ShellView>('GET', '/native/v1/shell'),
  put: (s: ShellSettings) => call<ShellView>('PUT', '/native/v1/shell', s),
  notify: (title: string, body: string) => call<{ ok: boolean }>('POST', '/native/v1/shell/notify', { title, body }),
  tooltip: (text: string) => call<{ ok: boolean }>('POST', '/native/v1/shell/tooltip', { text }),
  update: () => call<UpdateInfo>('GET', '/native/v1/update'),
}

/** Whether `now` lies in the person's quiet hours (`HH:MM` to `HH:MM`, possibly past midnight). */
export function inQuietHours(from: string, to: string, now = new Date()): boolean {
  const m = (s: string) => {
    const x = /^(\d{1,2}):(\d{2})$/.exec(s)
    return x ? Number(x[1]) * 60 + Number(x[2]) : null
  }
  const f = m(from)
  const t = m(to)
  if (f === null || t === null || f === t) return false
  const n = now.getHours() * 60 + now.getMinutes()
  return f < t ? n >= f && n < t : n >= f || n < t
}
