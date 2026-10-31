// Interface state shared across pages: which global dialog is open and the palette. Not data; nothing here is kept.

import { useEffect, useState } from 'preact/hooks'

export type DialogId = null | { kind: 'addUrl'; url?: string } | { kind: 'site' } | { kind: 'stop' } | { kind: 'downloadProgress'; jobId: string }

let dialog: DialogId = null
let palette: null | 'all' | 'themes' = null
const listeners = new Set<() => void>()

function emit(): void {
  for (const l of listeners) l()
}

export function openDialog(d: DialogId): void {
  dialog = d
  emit()
}

/** Follow a successful enqueue only if this pending download's dialog is still open. */
export function followEnqueuedDownload(pendingId: string, jobId: string): void {
  if (dialog?.kind === 'downloadProgress' && dialog.jobId === pendingId) {
    openDialog({ kind: 'downloadProgress', jobId })
  }
}

export function setPalette(p: null | 'all' | 'themes'): void {
  palette = p
  emit()
}

export function useUi(): { dialog: DialogId; palette: null | 'all' | 'themes' } {
  const [, setTick] = useState(0)
  useEffect(() => {
    const on = () => setTick((t) => t + 1)
    listeners.add(on)
    return () => {
      listeners.delete(on)
    }
  }, [])
  return { dialog, palette }
}

/** A short tone for completion or failure, drawn with the Web Audio API; nothing is fetched. */
export function chime(kind: 'done' | 'fail'): void {
  try {
    const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
    if (!Ctx) return
    const ctx = new Ctx()
    const o = ctx.createOscillator()
    const g = ctx.createGain()
    o.type = 'sine'
    o.frequency.value = kind === 'done' ? 880 : 220
    g.gain.setValueAtTime(0.0001, ctx.currentTime)
    g.gain.exponentialRampToValueAtTime(0.08, ctx.currentTime + 0.02)
    g.gain.exponentialRampToValueAtTime(0.0001, ctx.currentTime + 0.35)
    o.connect(g).connect(ctx.destination)
    o.start()
    o.stop(ctx.currentTime + 0.4)
    setTimeout(() => void ctx.close(), 600)
  } catch {
    // No audio device: stay silent.
  }
}
