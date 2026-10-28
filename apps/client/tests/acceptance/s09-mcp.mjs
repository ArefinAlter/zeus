// Scenario 9: the app's "Connect your AI assistant" page and a real MCP client.
//
// 1. The engine's own routes give the configuration for Claude Code, Cursor, Codex and a generic entry; each parses and
//    points at the bundled `zeus` and this app's environment.
// 2. "Test the connection" (a handshake started from Rust) lists the fourteen tools.
// 3. A separate MCP client over stdio (tests/mcp-client.mjs), started from the generated configuration exactly as an
//    assistant would, initializes, lists the tools, probes a URL, downloads a file from the fixture server, waits for it,
//    verifies it, and the SHA-256 matches the server's bytes.
// 4. Approvals stay human-only: no MCP tool decides one; a session request only creates a pending approval.
//
// Run: node tests/acceptance/s09-mcp.mjs   (ZEUS_BIN_DIR must hold the zeus, zeusd and zeus-native-host built together)

import { execFileSync } from 'node:child_process'
import { statSync } from 'node:fs'

import { startFixtures, sha256 } from '../fixture-server.mjs'
import { McpClient } from '../mcp-client.mjs'
import { startHarness, until } from '../harness.mjs'

const home = 'D:/verne-build/zeus/scratch/d/s09'
const results = []
const check = (name, ok, detail = '') => {
  results.push({ name, ok })
  console.log(`${ok ? 'PASS' : 'FAIL'}  ${name}${detail ? `  (${detail})` : ''}`)
}

const EXPECTED = ['zeus_probe', 'zeus_enqueue', 'zeus_status', 'zeus_wait', 'zeus_control', 'zeus_verify', 'zeus_session_list', 'zeus_session_request', 'zeus_files_list', 'zeus_cloud', 'zeus_workflow', 'zeus_files', 'zeus_artifacts', 'zeus_inbox']

const fx = await startFixtures(0)
const h = await startHarness({ home, fresh: true })
await h.ready()

// ---- 1. the configuration texts
const cfg = await h.get('/native/v1/mcp/config')
const engine = await h.get('/native/v1/engine')
check('the configuration points at the bundled zeus and says `mcp`', cfg.command.toLowerCase().includes(engine.bin.toLowerCase()) && cfg.args.join(' ') === 'mcp', cfg.command)
check('it carries the four variables that select this app\'s engine', ['ZEUS_PROFILE', 'ZEUS_STATE_DIR', 'ZEUS_CONFIG_DIR', 'ZEUS_RUNTIME_DIR'].every((k) => cfg.env[k] && cfg.env[k] === engine.env[k]))
const claude = JSON.parse(cfg.claude_code_json)
const cursor = JSON.parse(cfg.cursor_json)
const generic = JSON.parse(cfg.generic_json)
check('the Claude Code and Cursor JSON parse and name the same server', claude.mcpServers.zeus.command === cfg.command && cursor.mcpServers.zeus.command === cfg.command && generic.command === cfg.command)
check('the Claude Code command line is `claude mcp add ... zeus -- <zeus> mcp`', /^claude mcp add --transport stdio --scope user /.test(cfg.claude_code_cli) && cfg.claude_code_cli.includes(' zeus -- ') && cfg.claude_code_cli.endsWith(' mcp') && cfg.claude_code_cli.includes('ZEUS_STATE_DIR='))
check('the Codex entry is a [mcp_servers.zeus] table with command, args and env', cfg.codex_toml.startsWith('[mcp_servers.zeus]\n') && /command = /.test(cfg.codex_toml) && /args = \["mcp"\]/.test(cfg.codex_toml) && /ZEUS_PROFILE = /.test(cfg.codex_toml))

// ---- 2. the handshake the app itself runs
const test = await h.post('/native/v1/mcp/test', {})
check('the app\'s own connection test lists fourteen tools', test.ok && test.tool_count === 14, `${test.server?.name} ${test.server?.version}, ${test.ms} ms`)
check('they are the fourteen tools of the catalogue', EXPECTED.every((n) => test.tools.some((t) => t.name === n)))

// ---- 3. a real MCP client, started from the generated configuration
const client = McpClient.start({ command: claude.mcpServers.zeus.command, args: claude.mcpServers.zeus.args, env: claude.mcpServers.zeus.env })
const init = await client.initialize()
check('an MCP client handshake succeeds', !!init.serverInfo?.name, `${init.serverInfo?.name} ${init.serverInfo?.version}, protocol ${init.protocolVersion}`)
const tools = await client.listTools()
check('tools/list returns the fourteen tools', tools.length === 14 && EXPECTED.every((n) => tools.some((t) => t.name === n)))
const decides = tools.filter((t) => /approv|decid|grant|release|unlock/i.test(t.name))
check('no listed tool can decide an approval', decides.length === 0, decides.map((t) => t.name).join(',') || 'none')

const size = 12 * 1024 * 1024
const seed = 31
const url = `${fx.url}/files/from-an-assistant.bin?size=${size}&seed=${seed}`
const probe = await client.callTool('zeus_probe', { url })
check('zeus_probe reports the size and that ranges work', !probe.isError && JSON.stringify(probe.structured ?? probe.text).includes(String(size)), (probe.text || '').slice(0, 100))
const enq = await client.callTool('zeus_enqueue', { urls: [url], idempotency_key: `acc-${Date.now()}` })
const jobId = /job_[0-7][0-9A-HJKMNP-TV-Z]{25}/.exec(JSON.stringify(enq.structured) + enq.text)?.[0]
check('zeus_enqueue starts a download and returns a job id', !enq.isError && !!jobId, jobId)
const done = await until(async () => {
  const s = await client.callTool('zeus_status', { job_id: jobId })
  const text = JSON.stringify(s.structured ?? s.text)
  return /"state":"(completed|failed)"/.test(text) ? { text, structured: s.structured } : false
}, { timeoutMs: 120000, everyMs: 1000, what: 'the job to settle' })
const finalState = /"state":"(\w+)"/.exec(done.text)?.[1]
check('the download completes through the MCP tools', finalState === 'completed', finalState)
const fromJobs = (await h.get('/api/v1/jobs')).items.find((j) => j.id === jobId)
check('its SHA-256 equals the bytes the server holds', fromJobs?.sha256 === sha256(seed, size) && statSync(fromJobs.path).size === size, fromJobs?.sha256?.slice(0, 16))
const verify = await client.callTool('zeus_verify', { job_id: jobId, expected_sha256: sha256(seed, size) })
check('zeus_verify re-hashes the file and agrees', !verify.isError && /match|verified|ok|true/i.test(JSON.stringify(verify.structured ?? verify.text)), JSON.stringify(verify.structured ?? verify.text).slice(0, 100))
const listed = await client.callTool('zeus_files_list', {})
check('zeus_files_list shows the file', JSON.stringify(listed.structured ?? listed.text).includes('from-an-assistant.bin'))

// ---- 4. approvals stay human-only
// The app's own credential sees only approvals it requested (spec 05 7.5), so the listing below is the person's own
// command line: `zeus approvals list`, the read-only part of the human trust surface.
const cli = (args) => JSON.parse(execFileSync(cfg.command, [...args, '--json'], { env: { ...process.env, ...cfg.env }, encoding: 'utf8', windowsHide: true }))
const waiting = () => {
  const out = cli(['approvals', 'list'])
  return Array.isArray(out) ? out : (out.items ?? out.approvals ?? [])
}
const before = waiting().length
const sess = await client.callTool('zeus_session_request', { origin: new URL(fx.url).origin, scope: 'site', reason: 'Acceptance run: an assistant asking for a login.', idempotency_key: `sess-${Date.now()}` })
const pending = await until(async () => {
  const items = waiting()
  return items.length > before ? items : false
}, { timeoutMs: 15000, what: 'a pending approval' }).catch(() => [])
const target = pending[pending.length - 1]
check('a session request creates a pending approval and nothing more', !sess.isError && pending.length > before, `${pending.length} waiting for a person`)
const unknown = await client.callTool('zeus_approve', { approval_id: target?.id ?? 'ap_x' }).catch((e) => ({ isError: true, text: e.message }))
check('a call to an approving tool is refused as unknown', unknown.isError === true, (unknown.text || '').slice(0, 80))
const noRoute = await h.post('/native/v1/approvals/decide', { approval_id: target?.id }).catch((e) => e.code)
check('the app route table has no way to decide a daemon approval', noRoute === 'app.unknown_route', String(noRoute))
const still = waiting().find((a) => a.id === target?.id)
check('the approval is still waiting for a person after every attempt', !!still, still?.state ?? still?.status ?? 'listed')

await client.close()
await h.stop()
await fx.close()
const failed = results.filter((r) => !r.ok)
console.log(failed.length ? `SCENARIO 9: ${failed.length} FAILED` : 'SCENARIO 9: PASSED')
process.exit(failed.length ? 1 : 0)
