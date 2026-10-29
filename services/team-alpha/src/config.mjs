import { lstatSync, readFileSync } from 'node:fs'
import { features, id, integer, label, object, publicUrl, text, fail } from './contracts.mjs'
import { isAbsolute } from 'node:path'

export function validateConfig(input, { fixture = false } = {}) {
  object(input, ['origin', 'issuer', 'client_id', 'audience', 'client_secret', 'token_auth_method', 'oidc_origins', 'groups_claim', 'database', 'listen_host', 'listen_port', 'teams', 'signing'])
  const origin = publicUrl(input.origin, fixture)
  if (origin.pathname !== '/' || origin.href !== origin.origin + '/') fail('team.invalid_origin')
  const issuer = publicUrl(input.issuer, fixture)
  text(input.client_id, 512); text(input.audience, 512)
  if (input.audience !== input.client_id) fail('team.id_token_audience_must_match_client')
  if (!['client_secret_basic', 'client_secret_post'].includes(input.token_auth_method)) fail('team.invalid_client_auth')
  text(input.client_secret, 8192)
  if (!Array.isArray(input.oidc_origins) || input.oidc_origins.length < 1 || input.oidc_origins.length > 8) fail('team.invalid_oidc_origins')
  const origins = new Set(input.oidc_origins.map(raw => {
    const parsed = publicUrl(raw, fixture)
    if (parsed.pathname !== '/') fail('team.invalid_oidc_origin')
    return parsed.origin
  }))
  if (!origins.has(issuer.origin)) fail('team.issuer_origin_not_pinned')
  text(input.groups_claim, 128, /^[A-Za-z][A-Za-z0-9_:/.\-]*$/u)
  if (!Array.isArray(input.teams) || input.teams.length < 1 || input.teams.length > 100) fail('team.invalid_teams')
  const ids = new Set()
  for (const team of input.teams) {
    object(team, ['id', 'label', 'features', 'bootstrap_members', 'group_roles'])
    id(team.id); label(team.label); features(team.features)
    if (ids.has(team.id)) fail('team.duplicate_team'); ids.add(team.id)
    if (!Array.isArray(team.bootstrap_members) || team.bootstrap_members.length > 1000 || !Array.isArray(team.group_roles) || team.group_roles.length > 100) fail('team.invalid_membership')
    for (const member of team.bootstrap_members) {
      object(member, ['issuer', 'subject', 'role']); text(member.subject, 255)
      if (member.issuer !== input.issuer) fail('team.untrusted_member_issuer')
      if (!['reader', 'writer', 'admin'].includes(member.role)) fail('team.invalid_role')
    }
    for (const mapping of team.group_roles) {
      object(mapping, ['issuer', 'group', 'role']); text(mapping.group, 255)
      if (mapping.issuer !== input.issuer || !['reader', 'writer', 'admin'].includes(mapping.role)) fail('team.invalid_group_mapping')
    }
    if (!team.bootstrap_members.some(member => member.role === 'admin') && !team.group_roles.some(mapping => mapping.role === 'admin')) fail('team.admin_required')
  }
  object(input.signing, ['active_key', 'keys']); id(input.signing.active_key)
  if (!input.signing.keys || Object.keys(input.signing.keys).length < 1 || Object.keys(input.signing.keys).length > 8) fail('team.invalid_signing_keys')
  if (!['127.0.0.1', '0.0.0.0', '::1'].includes(input.listen_host)) fail('team.invalid_listener')
  integer(input.listen_port, fixture ? 0 : 1, 65535)
  text(input.database, 4096)
  if (!(fixture && input.database === ':memory:') && !isAbsolute(input.database)) fail('team.absolute_database_path_required')
  return { ...input, origin: origin.origin, issuer: input.issuer, oidcOrigins: origins, fixture }
}
export function runtime() {
  const [major, minor, patch] = process.versions.node.split('.').map(Number)
  if (major !== 24 || minor < 22 || (minor === 22 && patch < 1)) fail('team.node24_required', 500)
  if (process.env.NODE_TLS_REJECT_UNAUTHORIZED === '0' || process.env.NODE_USE_ENV_PROXY === '1') fail('team.unsafe_tls_or_proxy_environment', 500)
}
export function privateFile(path) {
  const stat = lstatSync(path)
  if (!stat.isFile() || stat.isSymbolicLink() || stat.size > 65536 || (process.platform !== 'win32' && (stat.mode & 0o077))) fail('team.unsafe_secret_file', 500)
  return readFileSync(path, 'utf8')
}
export function loadConfig(path, env = process.env) {
  runtime()
  const input = JSON.parse(privateFile(path))
  // Deployment JSON names paths/IDs only; private app/signing material comes from explicit inputs.
  if (Object.hasOwn(input, 'client_secret') || Object.hasOwn(input.signing ?? {}, 'keys')) fail('team.plaintext_secret_configuration', 500)
  const keys = Object.fromEntries(Object.entries(input.signing.key_files ?? {}).map(([kid, file]) => [kid, privateFile(file)]))
  input.client_secret = env.ZEUS_TEAM_OIDC_CLIENT_SECRET
  input.signing = { active_key: input.signing.active_key, keys }
  return validateConfig(input)
}
