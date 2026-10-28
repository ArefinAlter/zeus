// Connect: let an AI assistant, and a browser, use Zeus on this computer.
//
// The first part gives the exact configuration for each assistant, pointing at the `zeus` program installed beside this
// app and at this app's own engine, and tests it with a real MCP handshake. The second part is browser integration.
// Whatever an assistant can do is what the fifteen MCP tools allow; none of them can decide an approval.

import { useEffect, useState } from 'preact/hooks'

import type { McpConfig, McpTest } from '../backend/integration'
import { mcp } from '../backend/integration'
import { NATIVE } from '../backend/live'
import { ACCESS_INFO, ACCESS_ORDER, NEVER, TOOLS } from '../model/agentTools.ts'
import { href } from '../lib/router'
import { Chip, CodeCopy, Empty, Notice, PageHead, SettingsGroup, Tabs } from '../ui/components'
import { Icon } from '../ui/icons'
import { BrowserIntegration } from './integration/BrowserIntegration'
import '../styles/integration.css'

type Assistant = 'claude' | 'cursor' | 'codex' | 'other'

function Assistants({ cfg }: { cfg: McpConfig }) {
  const [tab, setTab] = useState<Assistant>('claude')
  return (
    <div class="connect-tabs">
      <Tabs
        value={tab}
        onChange={setTab}
        tabs={[
          ['claude', 'Claude Code'],
          ['cursor', 'Cursor'],
          ['codex', 'Codex'],
          ['other', 'Other assistant'],
        ]}
      />
      {tab === 'claude' ? (
        <>
          <ol class="connect-steps">
            <li>Run this once in a terminal. It adds Zeus for your user in every project:</li>
          </ol>
          <CodeCopy text={cfg.claude_code_cli} label="Copy command" />
          <p class="muted small">Or put this in {cfg.paths.claude_code_json}:</p>
          <CodeCopy text={cfg.claude_code_json} />
          <p class="muted small">Then start Claude Code and run /mcp: Zeus should be listed as connected.</p>
        </>
      ) : null}
      {tab === 'cursor' ? (
        <>
          <p class="small">Add this to {cfg.paths.cursor_json}, then restart Cursor (Settings, MCP shows Zeus with its tools):</p>
          <CodeCopy text={cfg.cursor_json} />
        </>
      ) : null}
      {tab === 'codex' ? (
        <>
          <p class="small">Add this to {cfg.paths.codex_toml}, then start Codex again:</p>
          <CodeCopy text={cfg.codex_toml} />
        </>
      ) : null}
      {tab === 'other' ? (
        <>
          <p class="small">Any assistant that starts MCP servers over standard input and output can use this entry. Zeus speaks only stdio; it opens no network port.</p>
          <CodeCopy text={cfg.generic_json} />
        </>
      ) : null}
      <p class="muted small">
        The variables select this app's engine, so the assistant talks to the same Zeus you see here. If your assistant's configuration format has changed, its own documentation wins: the command, the argument <span class="mono">mcp</span> and these four variables are what Zeus needs.
      </p>
    </div>
  )
}

function Test() {
  const [state, setState] = useState<'idle' | 'running' | 'done' | 'failed'>('idle')
  const [result, setResult] = useState<McpTest | null>(null)
  const [error, setError] = useState<string | null>(null)
  const run = () => {
    setState('running')
    setError(null)
    mcp.test().then(
      (r) => (setResult(r), setState('done')),
      (e: unknown) => (setError(e instanceof Error ? e.message : 'The test failed.'), setState('failed')),
    )
  }
  return (
    <div class="test-result">
      <div class="toolbar">
        <button type="button" class="btn" disabled={state === 'running'} onClick={run}>
          <Icon name="bolt" /> {state === 'running' ? 'Testing…' : 'Test the connection'}
        </button>
        <span class="muted small">Starts the installed zeus mcp the way an assistant does and asks it for its tools. Nothing is downloaded.</span>
      </div>
      {state === 'done' && result ? (
        <Notice kind={result.ok ? 'ok' : 'warn'}>
          {result.ok ? `Connected: ${result.server?.name ?? 'zeus'} ${result.server?.version ?? ''}, ${result.tool_count} tools, answered in ${result.ms} ms.` : 'The server answered but listed no tools.'}
          <div class="names">
            {result.tools.map((t) => (
              <Chip key={t.name} tone={t.read_only ? 'ok' : ''} title={t.title ?? ''}>
                {t.name}
              </Chip>
            ))}
          </div>
        </Notice>
      ) : null}
      {state === 'failed' ? <Notice kind="error">{error}</Notice> : null}
    </div>
  )
}

function Capabilities() {
  return (
    <>
      <div class="tool-groups">
        {ACCESS_ORDER.map((a) => (
          <div class="tool-group" key={a}>
            <h4>
              {ACCESS_INFO[a].label}
              <span class="muted small"> · {TOOLS.filter((t) => t.access === a).length} tools</span>
            </h4>
            <ul class="small">
              {TOOLS.filter((t) => t.access === a).map((t) => (
                <li key={t.name}>
                  <span class="mono">{t.name}</span>: {t.title.toLowerCase()}
                </li>
              ))}
            </ul>
          </div>
        ))}
      </div>
      <p class="muted small">A tool's access level is a limit this app's own agent applies. An outside assistant connects with the standard agent credential, which allows all fifteen tools; what it asks for still goes through the same checks in the engine.</p>
      <SettingsGroup title="What an assistant never does, whatever it is told" note="These hold for every assistant and every access level.">
        {NEVER.map((n) => (
          <div class="row">
            <div class="text">
              <span class="desc">{n}</span>
            </div>
          </div>
        ))}
      </SettingsGroup>
      <p class="small">
        When an assistant asks to use one of your logins, or to move your files, the request waits in <a href={href('approvals')}>Approvals</a>. You decide it in the browser extension's popup or at a terminal with <span class="mono">zeus approvals</span>; the assistant cannot.
      </p>
    </>
  )
}

export function Connect() {
  const [cfg, setCfg] = useState<McpConfig | null>(null)
  const [err, setErr] = useState<string | null>(null)
  useEffect(() => {
    if (!NATIVE) return
    mcp.config().then(setCfg, (e: unknown) => setErr(e instanceof Error ? e.message : 'The configuration could not be read.'))
  }, [])
  if (!NATIVE) {
    return (
      <div class="page">
        <PageHead title="Connect" sub="AI assistants and your browser." />
        <Empty>
          Connecting an assistant or a browser needs the Zeus desktop app, which runs its own engine on your computer. This page shows what it offers once you install it.
        </Empty>
      </div>
    )
  }
  return (
    <div class="page">
      <PageHead title="Connect" sub="Let an AI assistant, and your browser, use Zeus on this computer." />
      <SettingsGroup title="Connect your AI assistant" note="Assistants such as Claude Code, Cursor and Codex can check links, start downloads, watch them and verify the files, through Zeus's fifteen tools. They start the zeus program that was installed with this app.">
        {err ? <Notice kind="error">{err}</Notice> : null}
        {cfg ? (
          <div class="row stack">
            <Assistants cfg={cfg} />
            <Test />
          </div>
        ) : (
          <div class="row">
            <span class="muted">Reading the configuration…</span>
          </div>
        )}
      </SettingsGroup>
      <SettingsGroup title="What an assistant can do">
        <div class="row stack">
          <Capabilities />
        </div>
      </SettingsGroup>
      <SettingsGroup title="Browser integration" note="The Zeus extension hands over a download your browser gave up on, and lends a login only for a site you approve. It talks to this app's engine through a small host program.">
        <div class="row stack">
          <BrowserIntegration />
        </div>
      </SettingsGroup>
    </div>
  )
}
