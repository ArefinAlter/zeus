// The settings the scripted backend keeps for this session: the IDM-style options, the index options of a file search
// tool, and the agent and provider settings. Where zeusd has the same setting, `zeusdKey` names its configuration key
// (crates/zeus-daemon/src/config.rs) so a person can find it; the rest are marked planned in the interface.

import { defaultMap } from '../model/categories.ts'

export interface ProviderEntry {
  id: string
  name: string
  kind: 'openai' | 'anthropic' | 'google' | 'mistral' | 'deepseek' | 'qwen' | 'kimi' | 'glm' | 'ollama' | 'openai_compatible'
  enabled: boolean
  baseUrl: string
  model: string
  /** Held in memory for this session only, never written anywhere. */
  key?: string
  monthlyCapUsd: number
}

export interface SiteLogin {
  origin: string
  /** Shown name only. No user name or password value is ever stored by Zeus. */
  label: string
  scope: 'once' | 'site' | 'workflow' | 'never'
  expires?: number
  via: 'extension' | 'terminal'
}

export interface IndexRoot {
  path: string
  kind: 'ntfs' | 'refs' | 'folder' | 'network'
  include: boolean
  monitorChanges: boolean
  rescanEveryHours: number | null
  /** Native: one of the engine's download folders (it cannot be removed here). */
  download?: boolean
}

export interface Settings {
  general: {
    launchAtLogin: boolean
    startMinimized: boolean
    showTray: boolean
    closeToTray: boolean
    language: string
    checkUpdates: 'never' | 'weekly'
    timeFormat: '24h' | '12h'
  }
  categories: Record<string, string[]>
  saveTo: {
    root: string
    perCategory: boolean
    tempDir: string
    askEachTime: boolean
    rememberLast: boolean
    writeRoots: string[]
    /** An absolute folder per category that overrides its default subfolder (inside a download folder). */
    categoryDirs: Record<string, string>
    /** The last folder used for each category, kept when "remember the last folder" is on. */
    lastDirs: Record<string, string>
  }
  downloads: {
    concurrentJobs: number
    connectionsPerJob: number
    connectionsPerHost: number
    segmentMiB: number
    retries: number
    retryDelayS: number
    connectS: number
    readS: number
    probeS: number
    resume: 'verified_only' | 'ask' | 'restart'
    duplicates: 'rename' | 'fail' | 'replace'
    verify: 'sha256_blake3' | 'expected_only'
    showCompleteDialog: boolean
    startImmediately: boolean
    tombstoneDays: number
  }
  connection: {
    type: 'auto' | 'lan' | 'cellular'
    meteredPause: boolean
    proxy: 'system' | 'none' | 'http' | 'socks5'
    proxyHost: string
    proxyPort: number
    proxyBypass: string
    ipv6: boolean
    userAgent: 'zeus' | 'browser'
    ignoreTlsErrors: false
  }
  siteLogins: SiteLogin[]
  browser: {
    extension: 'connected' | 'not_installed'
    browsers: { name: string; enabled: boolean; profile: string }[]
    monitoredTypes: string
    minSizeKiB: number
    takeOverFailed: boolean
    hotkeyBypass: 'alt' | 'ctrl' | 'none'
    contextMenu: boolean
  }
  scheduler: {
    defaultQueue: string
    timezone: 'local' | 'utc'
    missedWindow: 'skip' | 'run_once'
    stopBehaviour: 'pause' | 'finish_current'
  }
  notifications: {
    onComplete: boolean
    onFailure: boolean
    onApproval: boolean
    onQueue: boolean
    quietFrom: string
    quietTo: string
    quietEnabled: boolean
  }
  index: {
    roots: IndexRoot[]
    exclusions: string[]
    excludeHidden: boolean
    excludeSystem: boolean
    indexSize: boolean
    indexDate: boolean
    fastSort: boolean
    contentIndex: boolean
    maxHistory: number
  }
  providers: ProviderEntry[]
  agent: {
    instructions: string
    defaultAccess: 'read' | 'download' | 'organize'
    toolsEnabled: string[]
    maxBatch: number
    maxTurnTools: number
    metadataOnly: boolean
  }
  storage: {
    keepTombstonesDays: number
    clearPartialAfterDays: number
    trashRetentionDays: number
    cleanupOnStart: boolean
  }
}

export function defaultSettings(): Settings {
  return {
    general: { launchAtLogin: false, startMinimized: false, showTray: true, closeToTray: true, language: 'English', checkUpdates: 'never', timeFormat: '24h' },
    categories: defaultMap(),
    saveTo: {
      root: 'D:\\Downloads\\Zeus',
      perCategory: true,
      tempDir: 'D:\\Downloads\\Zeus\\.zeus-partial',
      askEachTime: false,
      rememberLast: true,
      writeRoots: ['D:\\Downloads\\Zeus', 'E:\\Archive\\Zeus'],
      categoryDirs: {},
      lastDirs: {},
    },
    downloads: {
      concurrentJobs: 4,
      connectionsPerJob: 8,
      connectionsPerHost: 8,
      segmentMiB: 16,
      retries: 5,
      retryDelayS: 20,
      connectS: 15,
      readS: 30,
      probeS: 45,
      resume: 'verified_only',
      duplicates: 'rename',
      verify: 'sha256_blake3',
      showCompleteDialog: false,
      startImmediately: true,
      tombstoneDays: 30,
    },
    connection: { type: 'auto', meteredPause: true, proxy: 'system', proxyHost: '', proxyPort: 1080, proxyBypass: 'localhost, 127.0.0.1, *.local', ipv6: true, userAgent: 'zeus', ignoreTlsErrors: false },
    siteLogins: [
      { origin: 'https://library.example.edu', label: 'University library', scope: 'site', expires: Date.now() + 19 * 3600_000, via: 'extension' },
      { origin: 'https://files.example.net', label: 'Team file share', scope: 'once', via: 'extension' },
      { origin: 'https://ads.example.com', label: 'Blocked by you', scope: 'never', via: 'terminal' },
    ],
    browser: {
      extension: 'connected',
      browsers: [
        { name: 'Microsoft Edge', enabled: true, profile: 'Default' },
        { name: 'Google Chrome', enabled: true, profile: 'Profile 1' },
        { name: 'Firefox', enabled: false, profile: 'default-release' },
      ],
      monitoredTypes: 'zip rar 7z iso exe msi pdf epub mp4 mkv mp3 flac',
      minSizeKiB: 512,
      takeOverFailed: true,
      hotkeyBypass: 'alt',
      contextMenu: true,
    },
    scheduler: { defaultQueue: 'main', timezone: 'local', missedWindow: 'skip', stopBehaviour: 'pause' },
    notifications: { onComplete: true, onFailure: true, onApproval: true, onQueue: true, quietEnabled: false, quietFrom: '22:00', quietTo: '07:00' },
    index: {
      roots: [
        { path: 'D:\\Downloads\\Zeus', kind: 'ntfs', include: true, monitorChanges: true, rescanEveryHours: null },
        { path: 'E:\\Archive\\Zeus', kind: 'refs', include: true, monitorChanges: true, rescanEveryHours: null },
        { path: '\\\\nas.local\\shared\\downloads', kind: 'network', include: false, monitorChanges: false, rescanEveryHours: 24 },
      ],
      exclusions: ['*.zpart', '.zeus-partial', 'node_modules', 'Thumbs.db'],
      excludeHidden: true,
      excludeSystem: true,
      indexSize: true,
      indexDate: true,
      fastSort: true,
      contentIndex: false,
      maxHistory: 50,
    },
    providers: [
      { id: 'p-anthropic', name: 'Anthropic', kind: 'anthropic', enabled: false, baseUrl: 'https://api.anthropic.com', model: '', monthlyCapUsd: 10 },
      { id: 'p-openai', name: 'OpenAI', kind: 'openai', enabled: false, baseUrl: 'https://api.openai.com/v1', model: '', monthlyCapUsd: 10 },
      { id: 'p-google', name: 'Google', kind: 'google', enabled: false, baseUrl: 'https://generativelanguage.googleapis.com/v1beta', model: '', monthlyCapUsd: 10 },
      { id: 'p-ollama', name: 'Ollama on this computer', kind: 'ollama', enabled: false, baseUrl: 'http://127.0.0.1:11434', model: '', monthlyCapUsd: 0 },
    ],
    agent: {
      instructions: '',
      defaultAccess: 'read',
      toolsEnabled: [],
      maxBatch: 50,
      maxTurnTools: 12,
      metadataOnly: true,
    },
    storage: { keepTombstonesDays: 30, clearPartialAfterDays: 14, trashRetentionDays: 30, cleanupOnStart: false },
  }
}

/** Provider presets for the ladder: public base URLs only; nothing here is called by this release. */
export const PROVIDER_PRESETS: { kind: ProviderEntry['kind']; name: string; baseUrl: string; needsKey: boolean; note: string }[] = [
  { kind: 'anthropic', name: 'Anthropic', baseUrl: 'https://api.anthropic.com', needsKey: true, note: 'Messages API' },
  { kind: 'openai', name: 'OpenAI', baseUrl: 'https://api.openai.com/v1', needsKey: true, note: 'Responses API' },
  { kind: 'google', name: 'Google Gemini', baseUrl: 'https://generativelanguage.googleapis.com/v1beta', needsKey: true, note: 'Gemini API' },
  { kind: 'mistral', name: 'Mistral', baseUrl: 'https://api.mistral.ai/v1', needsKey: true, note: 'OpenAI-compatible chat' },
  { kind: 'deepseek', name: 'DeepSeek', baseUrl: 'https://api.deepseek.com', needsKey: true, note: 'OpenAI-compatible chat' },
  { kind: 'qwen', name: 'Qwen (DashScope)', baseUrl: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1', needsKey: true, note: 'OpenAI-compatible mode' },
  { kind: 'kimi', name: 'Kimi (Moonshot)', baseUrl: 'https://api.moonshot.ai/v1', needsKey: true, note: 'OpenAI-compatible chat' },
  { kind: 'glm', name: 'GLM (Z.ai)', baseUrl: 'https://api.z.ai/api/paas/v4', needsKey: true, note: 'OpenAI-compatible chat' },
  { kind: 'ollama', name: 'Ollama, local', baseUrl: 'http://127.0.0.1:11434', needsKey: false, note: 'Runs on this computer' },
  { kind: 'openai_compatible', name: 'Other OpenAI-compatible', baseUrl: 'https://', needsKey: false, note: 'Your own endpoint' },
]
