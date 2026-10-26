// Starts the app engine behind its loopback harness in a scratch folder, and talks to it. Used by the acceptance
// scripts (tests/acceptance/*.mjs). Nothing here touches a real profile: ZEUS_APP_HOME is always a scratch folder.
//
//   const h = await startHarness({ home })        // home: an absolute scratch folder
//   await h.ready()                                // waits until the engine answers
//   const jobs = await h.get('/api/v1/jobs')
//   await h.stop()                                 // clean shutdown (the engine stops zeusd)

import { spawn } from 'node:child_process'
import { existsSync, mkdirSync, rmSync } from 'node:fs'
import { join } from 'node:path'

export const BUILD = process.env.ZEUS_CLIENT_TARGET ?? 'A:\\zeus-build-cache\\zeus-client'
export const BIN = process.env.ZEUS_BIN_DIR ?? 'A:\\zeus-build-cache\\zeus\\debug'

export const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

export async function startHarness({ home, fresh = false, env = {} } = {}) {
  if (!home) throw new Error('home is required')
  if (fresh && existsSync(home)) rmSync(home, { recursive: true, force: true })
  mkdirSync(home, { recursive: true })
  const exe = join(BUILD, 'debug', process.platform === 'win32' ? 'zeus-app-http.exe' : 'zeus-app-http')
  const child = spawn(exe, [], {
    env: { ...process.env, ZEUS_APP_HOME: home, ZEUS_BIN_DIR: BIN, ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
    windowsHide: true,
  })
  let stderr = ''
  child.stderr.on('data', (d) => (stderr += d))
  const info = await new Promise((resolve, reject) => {
    let buf = ''
    child.stdout.on('data', (d) => {
      buf += d
      const line = buf.split('\n').find((l) => l.startsWith('{'))
      if (line) resolve(JSON.parse(line))
    })
    child.on('exit', (c) => reject(new Error(`harness exited ${c}: ${stderr}`)))
    setTimeout(() => reject(new Error('harness did not start')), 30000)
  })
  const call = async (method, path, body) => {
    const r = await fetch(info.url + path, {
      method,
      headers: { authorization: `Bearer ${info.token}`, ...(body !== undefined ? { 'content-type': 'application/json' } : {}) },
      ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
    })
    const data = await r.json().catch(() => null)
    if (!r.ok) throw Object.assign(new Error(data?.error?.message ?? `HTTP ${r.status}`), { code: data?.error?.code, status: r.status })
    return data
  }
  const h = {
    url: info.url,
    token: info.token,
    home,
    child,
    call,
    get: (p) => call('GET', p),
    post: (p, b = {}) => call('POST', p, b),
    put: (p, b) => call('PUT', p, b),
    async ready(timeoutMs = 90000) {
      const t0 = Date.now()
      while (Date.now() - t0 < timeoutMs) {
        try {
          const s = await call('GET', '/api/v1/status')
          if (s.engine?.phase === 'ready' && s.daemon) return s
          if (s.engine?.phase === 'failed') throw new Error(`engine failed: ${s.engine.message}`)
        } catch (e) {
          if (String(e.message).startsWith('engine failed')) throw e
        }
        await sleep(400)
      }
      throw new Error('the engine did not become ready')
    },
    /** Stop the harness cleanly (zeusd stops too); `kill` ends the app process at once and leaves zeusd running (a crash). */
    async stop({ kill = false } = {}) {
      if (kill) child.kill()
      else {
        await fetch(info.url + '/__harness/shutdown', { method: 'POST', headers: { authorization: `Bearer ${info.token}` } }).catch(() => undefined)
        setTimeout(() => child.kill(), 30000).unref()
      }
      await new Promise((r) => (child.exitCode !== null ? r() : child.once('exit', r)))
    },
    stderr: () => stderr,
  }
  return h
}

/** Poll `f` until it returns something truthy, or throw. */
export async function until(f, { timeoutMs = 60000, everyMs = 500, what = 'the condition' } = {}) {
  const t0 = Date.now()
  let last
  while (Date.now() - t0 < timeoutMs) {
    last = await f()
    if (last) return last
    await sleep(everyMs)
  }
  throw new Error(`timed out waiting for ${what}`)
}

/** Preview, then start, a list of items; resolves with the enqueue result. */
export async function download(h, items, key = `k${Date.now()}`) {
  const pv = await h.post('/api/v1/links/preview', { items })
  if (!pv.ready) throw new Error(`preview refused: ${JSON.stringify(pv.refused.length ? pv.refused : pv.items)}`)
  return h.post('/api/v1/links/enqueue', { review_id: pv.review_id, idempotency_key: key })
}
