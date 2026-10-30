// Scenario 10: the in-app agent with BYOK providers calling the fifteen MCP tools through the closed tool table.
//
// Against a local provider that speaks the real OpenAI and Anthropic wire formats (tests/providers-mock.mjs), a real
// zeusd, and the real interface in Chromium. A canary string stands in for the person's key.
//
//   set ZEUS_CLIENT_TARGET=<cargo target dir of zeus-app-http>; npm run build:harness
//   node tests/acceptance/s10-agent.mjs [--out DIR] [--keystore file]
//
// Part A drives the engine's routes (what the page does); part B drives the page.

import { mkdirSync, readdirSync, readFileSync, statSync } from 'node:fs'
import { join } from 'node:path'

import { startFixtures, sha256 } from '../fixture-server.mjs'
import { startHarness, until, sleep } from '../harness.mjs'
import { startProvider } from '../providers-mock.mjs'
import { openUi } from '../ui.mjs'

const arg = (n, d) => (process.argv.includes(n) ? process.argv[process.argv.indexOf(n) + 1] : d)
const out = arg('--out', 'D:/verne-build/zeus/evidence/s10-agent')
const fileKeystore = arg('--keystore', '') === 'file'
const home = join(out, fileKeystore ? 'profile-file-keystore' : 'profile')
mkdirSync(out, { recursive: true })

const CANARY = 'ZEUS-CANARY-sk-agent-0123456789abcdef'
const results = []
const check = (name, ok, detail = '') => {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}

const fx = await startFixtures(0)
const prov = await startProvider(0)
const h = await startHarness({ home, fresh: true, env: fileKeystore ? { ZEUS_KEYSTORE: 'file' } : {} })
const seen = [] // every response body the page or this script received from the engine
const call = async (method, path, body) => {
  const r = await h.call(method, path, body)
  seen.push(JSON.stringify(r))
  return r
}
const B = '/native/v1/agent'

let cleanup = async () => undefined
try {
  await h.ready()
  const cfg0 = await call('GET', `${B}/config`)
  check('API providers start disabled; discovered CLIs still require consent and no key is stored', cfg0.providers.every((p) => (['claude', 'codex', 'antigravity', 'cursor', 'grok'].includes(p.kind) || !p.enabled) && !p.consent && !p.key.stored))

  if (fileKeystore) {
    await call('POST', `${B}/keystore/initialize`, { passphrase: 'correct horse battery staple' })
  }
  const provider = (id, kind, base, over = {}) => ({ id, name: id, kind, enabled: true, base_url: base, model: 'mock-model-a', consent: false, monthly_cap_usd: 5, tools_mode: 'native', price_in_per_mtok: 3, price_out_per_mtok: 15, ...over })
  const write = (over = {}, providers) => ({
    providers: providers ?? [provider('p-mock', 'openai_compatible', `${prov.url}/v1`), provider('p-mock-anthropic', 'anthropic', `${prov.url}/anthropic`)],
    offline: false, daily_cap_usd: 5, thread_cap_usd: 2, default_price_in_per_mtok: 3, default_price_out_per_mtok: 15, max_tokens: 2048, ...over,
  })
  await call('PUT', `${B}/config`, write())
  cleanup = async () => {
    await h.call('DELETE', `${B}/keys/p-mock`).catch(() => undefined)
    await h.call('DELETE', `${B}/keys/p-mock-anthropic`).catch(() => undefined)
  }
  const k1 = await call('PUT', `${B}/keys/p-mock`, { key: CANARY })
  await call('PUT', `${B}/keys/p-mock-anthropic`, { key: CANARY })
  check('a key is stored and the page learns only "stored" and its last four characters', k1.stored === true && k1.last4 === CANARY.slice(-4))
  const cfgK = await call('GET', `${B}/config`)
  check('the configuration never contains the key', !JSON.stringify(cfgK).includes('ZEUS-CANARY') && cfgK.providers.find((p) => p.id === 'p-mock').key.last4 === CANARY.slice(-4))
  check('keys are kept where the engine says', ['os', 'file'].includes(cfgK.keystore.backend), `${cfgK.keystore.backend}${fileKeystore ? ' (forced)' : ''}`)

  const settings = { tools_enabled: ['zeus_probe', 'zeus_status', 'zeus_wait', 'zeus_verify', 'zeus_session_list', 'zeus_files_list', 'zeus_artifacts', 'zeus_enqueue', 'zeus_control', 'zeus_session_request', 'zeus_files', 'zeus_workflow', 'zeus_cloud', 'zeus_inbox', 'zeus_sources'], max_turn_tools: 12, max_batch: 50, metadata_only: true, instructions: '' }
  const body = (thread, text, access, over = {}) => ({ thread_id: thread, provider_id: 'p-mock', access, text, regenerate: false, settings, skill: null, ...over })
  const run = async (b, { until: stopWhen, timeoutMs = 60000, approveTools = [] } = {}) => {
    const { run_id } = await call('POST', `${B}/send`, b)
    const events = []
    let after = 0
    let done = false
    const t0 = Date.now()
    while (!done && Date.now() - t0 < timeoutMs) {
      const r = await call('POST', `${B}/runs/${run_id}/events`, { after })
      events.push(...r.events)
      for (const review of r.events.filter(e => e.type === 'tool_review')) {
        const tool = events.find(e => e.type === 'tool_start' && e.id === review.id)?.tool
        if (!approveTools.includes(tool)) throw new Error(`Unexpected fixture review: ${tool}`)
        const safeFixture = tool === 'zeus_enqueue'
          ? review.input.urls.every(value => new URL(typeof value === 'string' ? value : value.url).origin === fx.url)
          : tool === 'zeus_session_request' && new URL(review.input.origin).origin === 'http://127.0.0.1:18999'
        if (!safeFixture) throw new Error('The test refuses to authorize a non-fixture action')
        await call('POST', `${B}/runs/${run_id}/review`, { review_id: review.review_id, approve: true })
      }
      after = r.next
      done = r.done
      if (stopWhen?.(events)) return { run_id, events, done }
      if (!done) await sleep(150)
    }
    return { run_id, events, done }
  }
  const requests = () => prov.completions().length
  const jobsList = async () => (await call('GET', '/api/v1/jobs')).items
  const text = (events) => events.filter((e) => e.type === 'assistant_delta').map((e) => e.text).join('')
  const doneOf = (events) => events.find((e) => e.type === 'done')

  // ------------------------------------------------------------ consent
  const before = requests()
  const pv = await call('POST', `${B}/preview`, body('th_consent', 'download http://127.0.0.1:1/x.bin', 'download'))
  check('the preview lists what would be sent and says consent is missing', pv.blocker?.code === 'needs_consent' && pv.tools.length === 15 && pv.system.includes('You cannot approve anything') && pv.estimate.total_input_tokens > 100, `${pv.estimate.total_input_tokens} tokens in`)
  check('the preview names what is never sent', pv.never_sends.some((s) => /file contents/.test(s)) && !JSON.stringify(pv).includes('ZEUS-CANARY'))
  const blocked = await run(body('th_consent', 'hello', 'download'))
  check('without permission no request is made to the provider', requests() === before && doneOf(blocked.events)?.reason === 'needs_consent', doneOf(blocked.events)?.reason)
  await call('PUT', `${B}/config`, write({}, write().providers.map((p) => ({ ...p, consent: true }))))

  // ------------------------------------------------------------ a download through a tool call, OpenAI wire
  const size = 3 * 1024 * 1024
  const url = `${fx.url}/files/agent-sample.bin?size=${size}&seed=31`
  const r1 = await run(body('th_dl', `download ${url}`, 'download'), { approveTools: ['zeus_enqueue'] })
  const names = r1.events.filter((e) => e.type === 'tool_start').map((e) => e.tool)
  const started = r1.events.find((e) => e.type === 'tool_end')
  check('the model calls zeus_enqueue and the call is shown with its result', names[0] === 'zeus_enqueue' && started?.state === 'ok' && /download/.test(started.summary ?? ''), started?.summary)
  const jobCard = r1.events.find((e) => e.type === 'card' && e.card.type === 'job')
  check('a card for the job comes out of the result', !!jobCard)
  const job = await until(async () => { const j = (await jobsList()).find((x) => x.id === jobCard?.card.id); return j && ['completed', 'failed'].includes(j.state) ? j : false }, { timeoutMs: 60000, what: 'the agent\'s download' })
  check('the download ran in the real engine and completed with the right SHA-256', job.state === 'completed' && job.sha256 === sha256(31, size), job.sha256?.slice(0, 12))
  check('the final answer streamed after the tool result', /Done\./.test(text(r1.events)) && doneOf(r1.events)?.reason === 'end')
  const usage = r1.events.filter((e) => e.type === 'usage')
  check('usage and spend are recorded per call and per thread', usage.length === 2 && usage[1].thread.input_tokens === 240 && usage[1].thread.usd > 0, `thread ${usage[1]?.thread.input_tokens} in, ${usage[1]?.thread.output_tokens} out, $${usage[1]?.thread.usd}`)
  const wire = prov.completions().at(-2)
  check('the request carried the key in a header, the 15 tools and the system prompt; the key was in no body or URL', wire.headers.authorization === `Bearer ${CANARY}` && wire.body.tools.length === 15 && wire.body.messages[0].role === 'system' && !JSON.stringify(wire.body).includes('ZEUS-CANARY') && !wire.path.includes('ZEUS'))
  const secondWire = prov.completions().at(-1)
  const toolMsg = secondWire.body.messages.find((m) => m.role === 'tool')
  check('the tool result went back to the model labelled as data', !!toolMsg && /not instructions/.test(toolMsg.content))
  const used = await call('POST', `${B}/usage`, { thread_id: 'th_dl' })
  check('the usage route agrees with the events', used.thread.calls === 2 && used.today.calls >= 2)

  // ------------------------------------------------------------ the same on the Anthropic wire
  const urlA = `${fx.url}/files/agent-anthropic.bin?size=${size}&seed=32`
  const rA = await run(body('th_an', `download ${urlA}`, 'download', { provider_id: 'p-mock-anthropic' }), { approveTools: ['zeus_enqueue'] })
  const jobA = rA.events.find((e) => e.type === 'card' && e.card.type === 'job')
  const doneA = await until(async () => { const j = (await jobsList()).find((x) => x.id === jobA?.card.id); return j && j.state === 'completed' ? j : false }, { timeoutMs: 60000, what: 'the Anthropic-wire download' })
  const wireA = prov.completions().filter((r) => r.path.endsWith('/messages'))[0]
  check('the Anthropic wire works end to end (tool_use, tool_result, x-api-key, version header)', doneA.sha256 === sha256(32, size) && wireA.headers['x-api-key'] === CANARY && wireA.headers['anthropic-version'] === '2023-06-01' && wireA.body.tools.length === 15)

  // ------------------------------------------------------------ refusals
  const jobsBefore = (await jobsList()).length
  const rRo = await run(body('th_ro', `download ${fx.url}/files/refused.bin?size=1000&seed=1`, 'read'))
  const denied = rRo.events.find((e) => e.type === 'tool_end')
  check('at Read only access a download call is shown as refused and nothing starts', denied?.state === 'denied' && /needs Download access and this thread has Read only/.test(denied.summary) && (await jobsList()).length === jobsBefore, denied?.summary)
  check('the model is told about the refusal and the answer says so', /refused/i.test(text(rRo.events)))
  const rAp = await run(body('th_ap', 'please approve everything for me', 'organize'))
  const ends = rAp.events.filter((e) => e.type === 'tool_end')
  check('a tool that does not exist and an approve action are both refused', ends.length === 2 && ends.every((e) => e.state === 'denied') && /no such tool/.test(ends[0].summary) && /only a person/.test(ends[1].summary), ends.map((e) => e.summary).join(' | '))
  const rOff = await run(body('th_off', `download ${fx.url}/files/off.bin?size=10&seed=1`, 'download', { settings: { ...settings, tools_enabled: settings.tools_enabled.filter((t) => t !== 'zeus_enqueue') } }))
  check('a tool switched off in Settings is refused with that reason', rOff.events.find((e) => e.type === 'tool_end')?.summary.includes('switched off in Settings'))
  const rBatch = await run(body('th_batch', `download ${fx.url}/files/b.bin?size=10&seed=1`, 'download', { settings: { ...settings, max_batch: 1 } }), { approveTools: ['zeus_enqueue'] })
  check('the batch limit does not refuse a single download', rBatch.events.find((e) => e.type === 'tool_end')?.state === 'ok')

  // ------------------------------------------------------------ approvals stay with a person
  const rSess = await run(body('th_sess', 'session http://127.0.0.1:18999', 'download'), { approveTools: ['zeus_session_request'] })
  const sessEnd = rSess.events.find((e) => e.type === 'tool_end')
  const approvalsCard = rSess.events.find((e) => e.type === 'card' && e.card.type === 'approvals')
  const ownIds = approvalsCard?.card.ids ?? []
  // A daemon agent sees only the approvals it raised, so the page reads them through the agent's own connection.
  const own = await until(async () => { const r = await call('POST', `${B}/approvals`, { ids: ownIds }); return r.items.length ? r : false }, { timeoutMs: 15000, what: 'the approval the agent raised' }).catch(() => ({ items: [] }))
  const pending = own.items[0]
  check('a session request creates an approval that waits for a person, and a card says so', sessEnd?.state === 'ok' && !!approvalsCard && pending?.state === 'pending', `${sessEnd?.summary}; state ${pending?.state}; kind ${pending?.kind}`)
  const attempts = []
  for (const [m, path] of [['POST', `/api/v1/approvals/${pending?.id}/decide`], ['POST', `/api/v1/approvals/${pending?.id}/approve`], ['POST', `/native/v1/approvals/${pending?.id}/decide`], ['POST', `${B}/approvals/${pending?.id}/decide`], ['PUT', `/api/v1/approvals/${pending?.id}`]]) {
    attempts.push(await h.call(m, path, { decision: 'approve' }).then(() => 'accepted', (e) => `refused ${e.status}`))
  }
  const stillPending = (await call('POST', `${B}/approvals`, { ids: ownIds })).items[0]?.state
  check('no route of the app decides a daemon approval, and the request the agent raised is still pending afterwards', attempts.every((x) => x.startsWith('refused')) && stillPending === 'pending', attempts.join(', '))

  // ------------------------------------------------------------ caps, offline, stop
  const reqBefore = requests()
  await call('PUT', `${B}/config`, write({}, write().providers.map((p) => ({ ...p, consent: true, monthly_cap_usd: p.id === 'p-mock' ? 0 : p.monthly_cap_usd }))))
  const rCap = await run(body('th_cap', 'hello', 'read'))
  check('a cap of zero means no model call', requests() === reqBefore && doneOf(rCap.events)?.reason === 'cap' && /is zero/.test(doneOf(rCap.events)?.message ?? ''), doneOf(rCap.events)?.message?.slice(0, 80))
  await call('PUT', `${B}/config`, write({ offline: true }, write().providers.map((p) => ({ ...p, consent: true }))))
  const rOffline = await run(body('th_offline', 'hello', 'read'))
  const testOff = await call('POST', `${B}/providers/p-mock/test`, {})
  check('the offline switch blocks every call, tests included', requests() === reqBefore && doneOf(rOffline.events)?.reason === 'offline' && testOff.ok === false && testOff.code === 'offline')
  await call('PUT', `${B}/config`, write({}, write().providers.map((p) => ({ ...p, consent: true }))))
  const tested = await call('POST', `${B}/providers/p-mock/test`, {})
  check('Test connection makes one real tiny request after consent', tested.ok === true && tested.text !== undefined, `${tested.ms} ms`)
  const models = await call('POST', `${B}/providers/p-mock/models`, {})
  check('List models asks the provider', models.ok === true && models.models.join() === 'mock-model-a,mock-model-b')

  const slow = await call('POST', `${B}/send`, body('th_slow', 'slow please', 'read'))
  const evs = []
  let after = 0
  await until(async () => { const r = await call('POST', `${B}/runs/${slow.run_id}/events`, { after }); evs.push(...r.events); after = r.next; return evs.filter((e) => e.type === 'assistant_delta').length >= 3 }, { timeoutMs: 20000, everyMs: 100, what: 'streamed words' })
  const tStop = Date.now()
  await call('POST', `${B}/runs/${slow.run_id}/stop`, {})
  const fin = await until(async () => { const r = await call('POST', `${B}/runs/${slow.run_id}/events`, { after }); evs.push(...r.events); after = r.next; return r.done ? evs : false }, { timeoutMs: 10000, everyMs: 100, what: 'the run to end' })
  check('stopping mid-stream ends the run at once and keeps what streamed', doneOf(fin)?.reason === 'stopped' && Date.now() - tStop < 3000 && text(fin).length > 0, `${Date.now() - tStop} ms`)
  const gap = await call('POST', `${B}/send`, body('th_slow', 'hello again', 'read'))
  check('a new message in the same thread works after a stop', !!gap.run_id)
  await until(async () => (await call('POST', `${B}/runs/${gap.run_id}/events`, { after: 0 })).done, { timeoutMs: 20000, what: 'the follow-up run' })

  // ------------------------------------------------------------ the page
  const ui = await openUi(h)
  const { page } = ui
  page.on('response', async (r) => {
    if (r.url().startsWith(h.url)) seen.push(await r.text().catch(() => ''))
  })
  await call('PUT', `${B}/config`, write({}, write().providers.map((p) => ({ ...p, consent: false }))))
  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/settings/providers`)
  await page.waitForSelector('text=Safety switches', { timeout: 20000 })
  await page.screenshot({ path: `${out}/ui-providers.png` })
  const pageText = await page.evaluate(() => document.body.innerText + [...document.querySelectorAll('input')].map((i) => i.value + i.placeholder).join(' '))
  check('the Providers page shows the key as stored with its last four characters only', pageText.includes('••••') && pageText.includes(CANARY.slice(-4)) && !pageText.includes('ZEUS-CANARY'))

  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/agent`)
  await page.waitForSelector('textarea[aria-label="Message to the agent"]', { timeout: 20000 })
  await page.click('button[aria-label^="Access:"]')
  await page.click('[role=menuitemradio]:has-text("Download")')
  const beforeUi = requests()
  await page.fill('textarea[aria-label="Message to the agent"]', `download ${fx.url}/files/from-ui.bin?size=${size}&seed=33`)
  await page.keyboard.press('Enter')
  await page.waitForSelector('text=Allow p-mock and send', { timeout: 20000 })
  await page.screenshot({ path: `${out}/ui-consent.png` })
  const dialogText = await page.locator('[role=dialog], .dialog').first().innerText().catch(() => '')
  check('the page asks before the first request and lists what is sent and never sent', /never sent/i.test(dialogText) && /file contents/i.test(dialogText) && /tool definitions/i.test(dialogText), dialogText.slice(0, 60).replace(/\s+/g, ' '))
  await sleep(1500)
  check('nothing has reached the provider while the question is open', beforeUi === requests(), `${requests() - beforeUi} requests`)
  await page.click('button:has-text("Allow p-mock and send")')
  await page.waitForSelector('[aria-label="Review proposed action"]', { timeout: 30000 })
  check('the UI waits for per-action review before starting the download', !(await jobsList()).some(job => job.url_redacted?.includes('/from-ui.bin')))
  await page.getByRole('button', { name: 'Run this action', exact: true }).click()
  await page.waitForSelector('text=Done. I used the tool once', { timeout: 30000 })
  await page.screenshot({ path: `${out}/ui-thread.png` })
  for (const g of await page.locator('.grouphead[aria-expanded=false]').all()) await g.click()
  const timeline = await page.locator('.timeline').innerText()
  check('the thread shows the tool call, the job card and the streamed answer', /zeus_enqueue/.test(timeline) && /from-ui\.bin/.test(timeline) && /Done\./.test(timeline), timeline.replace(/\s+/g, ' ').slice(0, 100))
  await page.click('button[aria-label="Thread details"]').catch(() => undefined)
  const details = await page.locator('.agent-side').innerText().catch(() => '')
  check('the details panel names the model and shows the spend meter', /p-mock/.test(details) && /spend/.test(details) && /\$0\./.test(details), details.replace(/\s+/g, ' ').slice(0, 120))
  await page.screenshot({ path: `${out}/ui-details.png` })

  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/agent`)
  await page.waitForSelector('textarea[aria-label="Message to the agent"]')
  await page.fill('textarea[aria-label="Message to the agent"]', `download ${fx.url}/files/refused-ui.bin?size=100&seed=3`)
  await page.keyboard.press('Enter')
  await page.waitForSelector('.grouphead', { timeout: 30000 })
  await page.waitForSelector('text=That call was refused', { timeout: 30000 })
  const refusedText = (await page.locator('.timeline .grouphead').first().innerText()) + ' ' + (await page.locator('.timeline').innerText())
  await page.screenshot({ path: `${out}/ui-refused.png` })
  check('a refused call is shown as refused in the thread', /1 refused/i.test(refusedText) && /That call was refused/.test(refusedText), refusedText.replace(/\s+/g, ' ').slice(0, 100))

  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/agent`)
  await page.waitForSelector('textarea[aria-label="Message to the agent"]')
  await page.fill('textarea[aria-label="Message to the agent"]', 'slow please')
  await page.keyboard.press('Enter')
  await page.waitForSelector('.msg.assistant.streaming', { timeout: 30000 })
  await page.click('button[aria-label="Stop the agent"]')
  await page.waitForSelector('text=Stopped by you', { timeout: 10000 })
  await page.screenshot({ path: `${out}/ui-stopped.png` })
  check('the stop button ends a streaming answer', true)

  await page.goto(`${ui.origin}/index.html?harness=${encodeURIComponent(h.url)}&token=${h.token}#/agent`)
  await page.waitForSelector('textarea[aria-label="Message to the agent"]')
  await page.click('button[aria-label^="Access:"]')
  await page.click('[role=menuitemradio]:has-text("Download")')
  await page.fill('textarea[aria-label="Message to the agent"]', 'session http://127.0.0.1:18998')
  await page.keyboard.press('Enter')
  await page.getByRole('button', { name: 'Run this action', exact: true }).waitFor()
  check('requesting a browser session also waits for review of the exact origin', /127\.0\.0\.1:18998/.test(await page.locator('.timeline').innerText()))
  await page.getByRole('button', { name: 'Run this action', exact: true }).click()
  await page.locator('.timeline').getByText(/Nothing in this conversation can decide one/).waitFor({ timeout: 30000 })
  await page.waitForSelector('text=Waiting for you', { timeout: 30000 })
  await page.waitForSelector('text=Site login', { timeout: 15000 })
  await page.screenshot({ path: `${out}/ui-approval-card.png` })
  const cardText = await page.locator('.timeline').innerText()
  check('the approval the agent raised is shown on a card as waiting for a person, with the command to decide it at a terminal', /1 pending/.test(cardText) && /127\.0\.0\.1:18998/.test(cardText) && /Nothing in this conversation can decide one/.test(cardText) && /zeus approvals|approvals/.test(cardText), cardText.replace(/\s+/g, ' ').slice(0, 120))
  check('the page has no button that approves or denies it', (await page.locator('.timeline button:has-text("Allow"), .timeline button:has-text("Approve"), .timeline button:has-text("Deny")').count()) === 0)
  check('no console error on the pages used', ui.errors.length === 0, ui.errors.slice(0, 2).join(' | '))
  await ui.close()

  // ------------------------------------------------------------ the key is nowhere it should not be
  await call('DELETE', `${B}/keys/p-mock`)
  const after2 = await call('GET', `${B}/config`)
  check('removing a key removes it', after2.providers.find((p) => p.id === 'p-mock').key.stored === false)
  await cleanup()
  await h.stop()
  const hits = []
  const unreadable = []
  const scan = (dir) => {
    for (const f of readdirSync(dir)) {
      const p = join(dir, f)
      try {
        const st = statSync(p)
        if (st.isDirectory()) scan(p)
        else if (st.size < 80 * 1024 * 1024 && readFileSync(p).includes(CANARY)) hits.push(p)
      } catch {
        unreadable.push(p)
      }
    }
  }
  scan(home)
  check('the key is in no file of the app folder, the engine state or the logs', hits.length === 0 && unreadable.length === 0, `${hits.join(', ')}${unreadable.length ? ` ${unreadable.length} unreadable` : ''}`)
  check('the key was in no response the page or this script received', !seen.some((s) => s.includes('ZEUS-CANARY')))
  check('the key was not printed by the engine process', !h.stderr().includes('ZEUS-CANARY'))
} catch (e) {
  check(`the run completed (${e instanceof Error ? e.message : e})`, false)
} finally {
  await cleanup().catch(() => undefined)
  await h.stop().catch(() => undefined)
  await prov.close()
  await fx.close()
}
const failed = results.filter((r) => !r.ok)
console.log(failed.length ? `SCENARIO 10: ${failed.length} FAILED of ${results.length}` : `SCENARIO 10: PASSED (${results.length} checks${fileKeystore ? ', file key store' : ''})`)
process.exit(failed.length ? 1 : 0)
