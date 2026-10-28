import { loadConfig } from '../src/config.mjs'
import { Publisher } from '../src/signatures.mjs'
try {
  const config = loadConfig(process.env.ZEUS_TEAM_CONFIG)
  new Publisher({ issuer: config.origin, activeKey: config.signing.active_key, keys: config.signing.keys })
  console.log('Team configuration and signing keys validated; no network request or database mutation performed.')
} catch { console.error('team.configuration_unavailable'); process.exitCode = 1 }
