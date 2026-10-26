// A local model provider for the acceptance runs. It speaks two real wire formats, OpenAI Chat Completions
// (`POST /v1/chat/completions`, SSE `data:` chunks, `tool_calls` deltas, a usage chunk, `[DONE]`) and the Anthropic Messages API
// (`POST /anthropic/v1/messages`, SSE `message_start` / `content_block_*` / `message_delta` events), and a model list.
// It follows a small script keyed on what the person wrote, so a run is repeatable:
//
//   "download <url>"    calls zeus_enqueue for the url, then reports the result it gets back
//   "session <origin>"  calls zeus_session_request for the origin (an approval only a person can decide)
//   "approve ..."       tries to call a tool that decides approvals (there is none) and zeus_control with action approve
//   "slow ..."          streams a long answer slowly, so a run can be stopped in the middle
//   anything else       a short plain answer
//
// Every request is logged (method, path, header names, the body) at `/__log`, so a test can assert what was and was not
// sent. The log holds the key the client sent, as any provider would see it; tests look for it everywhere else.

import { createServer } from 'node:http'

const sleep = (ms) => new Promise((r) => setTimeout(r, ms))

function lastUserText(messages) {
  for (let i = messages.length - 1; i >= 0; i--) {
    const m = messages[i]
    if (m.role === 'user') {
      const c = m.content
      if (typeof c === 'string') return c
      if (Array.isArray(c)) {
        const t = c.filter((b) => b.type === 'text').map((b) => b.text).join('\n')
        if (t) return t
      }
    }
  }
  return ''
}

/** What the script does next, from the transcript in either wire format. */
function brain(wire, body) {
  const messages = body.messages ?? []
  const last = messages[messages.length - 1]
  const asToolResult = (() => {
    if (!last) return null
    if (wire === 'openai' && last.role === 'tool') return String(last.content)
    if (wire === 'anthropic' && last.role === 'user' && Array.isArray(last.content)) {
      const r = last.content.find((b) => b.type === 'tool_result')
      return r ? String(typeof r.content === 'string' ? r.content : JSON.stringify(r.content)) : null
    }
    return null
  })()
  if (asToolResult !== null) {
    if (asToolResult.startsWith('REFUSED')) return { text: `That call was refused, so I did nothing. ${asToolResult.slice(0, 160)}`, usage: [120, 24] }
    if (asToolResult.startsWith('ERROR')) return { text: 'The tool reported an error; I will not retry it.', usage: [120, 14] }
    return { text: 'Done. I used the tool once and nothing else; the result is in the card above.', usage: [140, 20] }
  }
  const text = lastUserText(messages)
  const url = /download\s+(https?:\/\/\S+)/i.exec(text)?.[1]
  if (url) return { text: 'I will start that download.', call: { name: 'zeus_enqueue', args: { urls: [url] } }, usage: [100, 30] }
  const origin = /session\s+(https?:\/\/\S+)/i.exec(text)?.[1]
  if (origin) return { text: '', call: { name: 'zeus_session_request', args: { origin, scope: 'site', reason: 'The files on this site need the person to be signed in.' } }, usage: [100, 30] }
  if (/\bapprove\b/i.test(text)) return { text: '', calls: [{ name: 'zeus_approve', args: { approval_id: 'ap_00000000000000000000000000' } }, { name: 'zeus_control', args: { action: 'approve', job_id: 'job_00000000000000000000000000' } }], usage: [100, 30] }
  if (/\bslow\b/i.test(text)) return { text: Array.from({ length: 80 }, (_, i) => `word${i} `).join(''), slow: true, usage: [100, 80] }
  return { text: 'I can check links and start downloads. Tell me what you need.', usage: [90, 14] }
}

const tcid = (n) => `call_${n}`

async function openai(req, res, body, state) {
  const plan = brain('openai', body)
  res.writeHead(200, { 'content-type': 'text/event-stream', 'x-ratelimit-remaining-requests': '99', 'x-ratelimit-remaining-tokens': '99000' })
  const send = (o) => res.write(`data: ${JSON.stringify(o)}\n\n`)
  const calls = plan.calls ?? (plan.call ? [plan.call] : [])
  const words = plan.text.match(/\S+\s*/g) ?? []
  for (const w of words) {
    if (state.closed) return
    send({ choices: [{ delta: { content: w } }] })
    if (plan.slow) await sleep(120)
  }
  calls.forEach((c, i) => {
    const args = JSON.stringify(c.args)
    send({ choices: [{ delta: { tool_calls: [{ index: i, id: tcid(state.n++), type: 'function', function: { name: c.name, arguments: '' } }] } }] })
    for (let p = 0; p < args.length; p += 24) send({ choices: [{ delta: { tool_calls: [{ index: i, function: { arguments: args.slice(p, p + 24) } }] } }] })
  })
  send({ choices: [{ delta: {}, finish_reason: calls.length ? 'tool_calls' : 'stop' }] })
  send({ choices: [], usage: { prompt_tokens: plan.usage[0], completion_tokens: plan.usage[1] } })
  res.write('data: [DONE]\n\n')
  res.end()
}

async function anthropic(req, res, body, state) {
  const plan = brain('anthropic', body)
  res.writeHead(200, { 'content-type': 'text/event-stream', 'anthropic-ratelimit-requests-remaining': '99' })
  const ev = (name, o) => res.write(`event: ${name}\ndata: ${JSON.stringify({ type: name, ...o })}\n\n`)
  ev('message_start', { message: { id: 'msg_1', role: 'assistant', content: [], usage: { input_tokens: plan.usage[0], output_tokens: 1 } } })
  let index = 0
  if (plan.text) {
    ev('content_block_start', { index, content_block: { type: 'text', text: '' } })
    for (const w of plan.text.match(/\S+\s*/g) ?? []) {
      if (state.closed) return
      ev('content_block_delta', { index, delta: { type: 'text_delta', text: w } })
      if (plan.slow) await sleep(120)
    }
    ev('content_block_stop', { index })
    index++
  }
  const calls = plan.calls ?? (plan.call ? [plan.call] : [])
  for (const c of calls) {
    ev('content_block_start', { index, content_block: { type: 'tool_use', id: `toolu_${state.n++}`, name: c.name, input: {} } })
    const args = JSON.stringify(c.args)
    for (let p = 0; p < args.length; p += 24) ev('content_block_delta', { index, delta: { type: 'input_json_delta', partial_json: args.slice(p, p + 24) } })
    ev('content_block_stop', { index })
    index++
  }
  ev('message_delta', { delta: { stop_reason: calls.length ? 'tool_use' : 'end_turn' }, usage: { output_tokens: plan.usage[1] } })
  ev('message_stop', {})
  res.end()
}

export async function startProvider(port = 0) {
  const log = []
  const state = { n: 1 }
  const server = createServer(async (req, res) => {
    const url = new URL(req.url ?? '/', 'http://x')
    if (url.pathname === '/__log') {
      res.writeHead(200, { 'content-type': 'application/json' })
      return res.end(JSON.stringify(log))
    }
    const chunks = []
    for await (const c of req) chunks.push(c)
    let body = null
    try {
      body = JSON.parse(Buffer.concat(chunks).toString('utf8') || 'null')
    } catch {
      body = null
    }
    log.push({ method: req.method, path: url.pathname, headers: req.headers, body, t: Date.now() })
    const s = { n: state.n, closed: false, get closedFlag() { return this.closed } }
    req.on('close', () => { s.closed = true })
    try {
      if (req.method === 'GET' && url.pathname.endsWith('/models')) {
        res.writeHead(200, { 'content-type': 'application/json' })
        return res.end(JSON.stringify({ data: [{ id: 'mock-model-a' }, { id: 'mock-model-b' }] }))
      }
      if (req.method === 'POST' && url.pathname === '/v1/chat/completions') {
        if (!String(req.headers.authorization ?? '').startsWith('Bearer ')) {
          res.writeHead(401, { 'content-type': 'application/json' })
          return res.end(JSON.stringify({ error: { message: 'no key' } }))
        }
        await openai(req, res, body, s)
        state.n = s.n
        return undefined
      }
      if (req.method === 'POST' && url.pathname === '/anthropic/v1/messages') {
        if (!req.headers['x-api-key']) {
          res.writeHead(401, { 'content-type': 'application/json' })
          return res.end(JSON.stringify({ type: 'error', error: { type: 'authentication_error', message: 'no key' } }))
        }
        await anthropic(req, res, body, s)
        state.n = s.n
        return undefined
      }
      res.writeHead(404)
      return res.end('not found')
    } catch {
      res.destroy()
      return undefined
    }
  })
  return new Promise((resolve) =>
    server.listen(port, '127.0.0.1', () => {
      const { port: p } = server.address()
      resolve({
        url: `http://127.0.0.1:${p}`,
        port: p,
        log,
        completions: () => log.filter((r) => r.path.endsWith('/chat/completions') || r.path.endsWith('/messages')),
        close: () => new Promise((r) => (server.closeAllConnections?.(), server.close(() => r()))),
      })
    }),
  )
}

if (process.argv[1]?.endsWith('providers-mock.mjs')) {
  const p = await startProvider(Number(process.argv[2] ?? 0))
  console.log(JSON.stringify({ url: p.url }))
}
