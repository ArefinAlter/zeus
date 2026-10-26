import { defineConfig, type Plugin } from 'vite'

// The Tauri shell freezes Object.prototype (`freezePrototype` in tauri.conf.json). A frozen prototype makes
// `{}.constructor = x` throw in strict mode, and Preact does exactly that when a component returns a top-level
// fragment: it copies the vnode, whose own `constructor: undefined` member is an ordinary key, with
// `for (k in src) dst[k] = src[k]`. The webview then stays blank. This keeps the freeze and changes only that copy:
// a key the prototype already has is defined on the copy instead of assigned. If a Preact upgrade changes the copy,
// the build fails rather than ship a blank window. (The same fix as Ogun Studio's build.)
const ASSIGN = 'function m(n,l){for(var u in l)n[u]=l[u];return n}'
const ASSIGN_SAFE =
  'function m(n,l){for(var u in l)u in Object.prototype?Object.defineProperty(n,u,{value:l[u],writable:!0,enumerable:!0,configurable:!0}):n[u]=l[u];return n}'

function preactFrozenPrototype(): Plugin {
  return {
    name: 'zeus:preact-frozen-prototype',
    enforce: 'pre',
    transform(code, id) {
      if (!/[\/]preact[\/]dist[\/]preact\.module\.js$/.test(id.split('?')[0] ?? '')) return null
      if (!code.includes(ASSIGN)) {
        throw new Error('preact.module.js no longer has the object copy this build patches (vite.config.ts); check it against a frozen Object.prototype and update the plugin')
      }
      return { code: code.replace(ASSIGN, ASSIGN_SAFE), map: null }
    },
  }
}

// One build serves the desktop shell (bundled assets) and the website copy under /client/. Relative asset URLs keep
// it independent of the mount point. Nothing is fetched at run time: no fonts, no scripts, no data.
// Build modes (`vite build --mode <mode>`), set here rather than in .env files so a fresh clone builds the same:
// * desktop (npm run build): the desktop app's frontend; inside the app it starts on the Zeus engine on this computer,
//   switchable in Settings.
// * harness: test builds of the interface against the engine's loopback harness (page address names it).
// * hosted: the web client the hosted pre-alpha serves on its own origin; no browser storage (the service keeps
//   preferences per sandbox).
// * scripted: offline, on the in-page sample backend; no network request at all.
// * site: the old website copy on the sample backend, without browser storage.
const BACKEND: Record<string, string> = { desktop: 'desktop', harness: 'harness', hosted: 'hosted', scripted: 'scripted', site: 'scripted', development: 'scripted' }
const NO_STORAGE = new Set(['hosted', 'site'])

export default defineConfig(({ mode }) => ({
  base: './',
  define: {
    __APP_VERSION__: JSON.stringify(process.env.npm_package_version ?? '0.0.1-alpha'),
    'import.meta.env.VITE_ZEUS_BACKEND': JSON.stringify(process.env.VITE_ZEUS_BACKEND ?? BACKEND[mode] ?? 'scripted'),
    'import.meta.env.VITE_ZEUS_NO_STORAGE': JSON.stringify(NO_STORAGE.has(mode) ? '1' : (process.env.VITE_ZEUS_NO_STORAGE ?? '')),
  },
  plugins: [preactFrozenPrototype()],
  optimizeDeps: { exclude: ['preact', 'preact/hooks', 'preact/jsx-runtime'] },
  esbuild: { jsx: 'automatic', jsxImportSource: 'preact' },
  build: {
    outDir: 'dist/web',
    emptyOutDir: true,
    target: 'es2022',
    sourcemap: false,
    // Images ship as files, never as data: URIs, so a strict CSP needs no exceptions.
    assetsInlineLimit: 0,
    modulePreload: { polyfill: false },
  },
  server: { port: 5174, strictPort: true },
  clearScreen: false,
}))
