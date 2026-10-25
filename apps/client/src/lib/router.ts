// The hash router: `#/downloads?cat=video`. Pages are addressable, so the back button and links work in the desktop
// webview and on the website alike.

import { useEffect, useState } from 'preact/hooks'

export interface Route {
  path: string[]
  query: URLSearchParams
}

function parseHash(): Route {
  const raw = window.location.hash.replace(/^#/, '') || '/'
  const [p, q] = raw.split('?', 2)
  return {
    path: (p ?? '/').split('/').filter(Boolean).map((s) => {
      try {
        return decodeURIComponent(s)
      } catch {
        return s
      }
    }),
    query: new URLSearchParams(q ?? ''),
  }
}

export function useRoute(): Route {
  const [route, setRoute] = useState(parseHash)
  useEffect(() => {
    const on = () => setRoute(parseHash())
    window.addEventListener('hashchange', on)
    return () => window.removeEventListener('hashchange', on)
  }, [])
  return route
}

/** Builds a hash link: href('downloads') or href('downloads', {cat: 'video'}). */
export function href(...parts: (string | Record<string, string | number | undefined>)[]): string {
  const segs: string[] = []
  let q = ''
  for (const p of parts) {
    if (typeof p === 'string') segs.push(encodeURIComponent(p))
    else {
      const sp = new URLSearchParams()
      for (const [k, v] of Object.entries(p)) if (v !== undefined && v !== '') sp.set(k, String(v))
      const s = sp.toString()
      if (s) q = '?' + s
    }
  }
  return '#/' + segs.join('/') + q
}

export function navigate(to: string): void {
  window.location.hash = to.replace(/^#/, '')
}
