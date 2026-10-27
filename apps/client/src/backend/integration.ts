// The desktop app's integration routes: the browser extension and its native-messaging host, and the MCP server that
// AI assistants start. Served by the app's engine (crates/zeus-app-integration); there is nothing like it in the
// hosted pre-alpha or the sample data, so the page that uses this module is shown for the desktop app only.

import { call } from './remote'

export interface BrowserState {
  id: 'chrome' | 'edge' | 'brave' | 'chromium' | 'firefox'
  name: string
  installed: boolean
  image: string | null
  /** The host can verify this install as the parent of a connection (a machine-wide install). */
  recognized_by_host: boolean
  extensions_page: string
  extension_id: string | null
  family: 'chromium' | 'firefox'
  manifest_file: string
  registry_key: string | null
  registered: boolean
  points_at_this_app: boolean
  extension_pinned: boolean
}

export interface IntegrationStatus {
  os: string
  extension: {
    packaged: boolean
    meta: { version?: string; version_name?: string; chromium_dev_id?: string; firefox_id?: string | null; note?: string; files?: Record<string, { bytes: number; sha256: string }> }
    chromium_folder: string
    firefox_folder: string
    unpacked: boolean
  }
  host: {
    exe: string
    exe_present: boolean
    launcher: string
    launcher_present: boolean
    log: string
    allow_list: string
    allow_list_present: boolean
    dev_extensions: string[]
    production_extensions: string[]
  }
  extension_connected: boolean | null
  notes: { development_id: string; firefox: string; recognized: string }
  browsers: BrowserState[]
}

export interface InstallPlan {
  browsers: string[]
  steps: { action: string; target: string; value?: string; note?: string }[]
  development_extension_ids: string[]
  consent_required: string[]
}

export interface McpConfig {
  command: string
  args: string[]
  env: Record<string, string>
  claude_code_cli: string
  claude_code_json: string
  cursor_json: string
  codex_toml: string
  generic_json: string
  paths: { claude_code_json: string; cursor_json: string; codex_toml: string }
}

export interface McpTest {
  ok: boolean
  server?: { name?: string; version?: string } | null
  protocol_version?: string | null
  tool_count: number
  tools: { name: string; title?: string | null; read_only?: boolean | null }[]
  ms: number
}

export const integration = {
  status: () => call<IntegrationStatus>('GET', '/native/v1/integration'),
  plan: (browsers: string[]) => call<InstallPlan>('POST', '/native/v1/integration/plan', { browsers }),
  install: (browsers: string[], consent: { development_extension_id: boolean }) => call<{ installed: string[] }>('POST', '/native/v1/integration/install', { browsers, consent }),
  uninstall: (browsers: string[]) => call<{ removed: string[] }>('POST', '/native/v1/integration/uninstall', { browsers }),
  unpack: () => call<{ chromium: string; firefox: string }>('POST', '/native/v1/integration/unpack', {}),
  exportTo: (dir: string) => call<{ folder: string; written: string[] }>('POST', '/native/v1/integration/export', { dir }),
}

export const mcp = {
  config: () => call<McpConfig>('GET', '/native/v1/mcp/config'),
  test: () => call<McpTest>('POST', '/native/v1/mcp/test', {}),
}
