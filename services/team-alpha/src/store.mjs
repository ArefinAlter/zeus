import { DatabaseSync } from 'node:sqlite'
import { chmodSync, lstatSync, mkdirSync } from 'node:fs'
import { dirname } from 'node:path'
import { actorId, canonical, fail, features, id, integer, jsonBytes, metadata, random, roleAtLeast, sha, utc } from './contracts.mjs'

const MAX_RECORDS = 10000
const MAX_CHANGES = 50000
const MAX_TENANT_BYTES = 16 * 1024 * 1024
const identityBytes = identity => canonical({ issuer: identity.issuer, subject: identity.subject, groups: identity.groups ?? [], oidc_expires_at_ms: identity.oidc_expires_at_ms })
const result = row => row && ({ kind: row.kind, record_id: row.id, revision: row.revision, sequence: row.seq, tombstone: Boolean(row.deleted), value: row.deleted ? null : JSON.parse(row.json) })

export class Store {
  constructor(path, configuredTeams, clock = Date.now) {
    this.clock = clock
    if (path !== ':memory:') {
      mkdirSync(dirname(path), { recursive: true, mode: 0o700 })
      try { if (lstatSync(path).isSymbolicLink()) fail('team.unsafe_state_path', 500) } catch (error) { if (error.code !== 'ENOENT') throw error }
    }
    this.db = new DatabaseSync(path, { enableForeignKeyConstraints: true, allowExtension: false, timeout: 3000 })
    if (path !== ':memory:') chmodSync(path, 0o600)
    this.db.exec(`PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA max_page_count=65536; PRAGMA journal_size_limit=4194304; PRAGMA wal_autocheckpoint=1000;
      CREATE TABLE IF NOT EXISTS teams(id TEXT PRIMARY KEY,revision INTEGER NOT NULL,features TEXT NOT NULL,seq INTEGER NOT NULL DEFAULT 0,policy_seq INTEGER NOT NULL DEFAULT 1,audit_seq INTEGER NOT NULL DEFAULT 0,audit_hash TEXT NOT NULL DEFAULT '',bytes INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS members(team TEXT NOT NULL,actor TEXT NOT NULL,issuer TEXT NOT NULL,subject TEXT NOT NULL,role TEXT,revoked INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(team,actor));
      CREATE TABLE IF NOT EXISTS sessions(hash TEXT PRIMARY KEY,team TEXT NOT NULL,identity TEXT NOT NULL,expires INTEGER NOT NULL,revoked INTEGER NOT NULL DEFAULT 0);
      CREATE TABLE IF NOT EXISTS devices(hash TEXT PRIMARY KEY,user_hash TEXT UNIQUE NOT NULL,id TEXT NOT NULL,alias TEXT NOT NULL,challenge TEXT NOT NULL,created INTEGER NOT NULL,expires INTEGER NOT NULL,state TEXT NOT NULL,approved TEXT,team TEXT,next_poll INTEGER NOT NULL);
      CREATE TABLE IF NOT EXISTS records(team TEXT NOT NULL,kind TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,seq INTEGER NOT NULL,deleted INTEGER NOT NULL,json TEXT NOT NULL,PRIMARY KEY(team,kind,id));
      CREATE TABLE IF NOT EXISTS changes(team TEXT NOT NULL,seq INTEGER NOT NULL,kind TEXT NOT NULL,id TEXT NOT NULL,revision INTEGER NOT NULL,deleted INTEGER NOT NULL,json TEXT NOT NULL,PRIMARY KEY(team,seq));
      CREATE TABLE IF NOT EXISTS receipts(team TEXT NOT NULL,actor TEXT NOT NULL,method TEXT NOT NULL,key TEXT NOT NULL,hash TEXT NOT NULL,result TEXT NOT NULL,PRIMARY KEY(team,actor,method,key));
      CREATE TABLE IF NOT EXISTS audit(team TEXT NOT NULL,seq INTEGER NOT NULL,prev TEXT NOT NULL,hash TEXT NOT NULL,json TEXT NOT NULL,PRIMARY KEY(team,seq));
      CREATE TABLE IF NOT EXISTS policy_leases(team TEXT PRIMARY KEY,seq INTEGER NOT NULL,expires INTEGER NOT NULL,json TEXT NOT NULL);`)
    this.configuredTeams = configuredTeams
    for (const team of configuredTeams) {
      this.db.prepare('INSERT OR IGNORE INTO teams(id,revision,features) VALUES (?,1,?)').run(team.id, canonical(features(team.features)))
      for (const member of team.bootstrap_members) this.db.prepare('INSERT OR IGNORE INTO members(team,actor,issuer,subject,role) VALUES (?,?,?,?,?)').run(team.id, actorId(member), member.issuer, member.subject, member.role)
    }
  }
  close() { this.db.close() }
  transaction(fn) {
    this.db.exec('BEGIN IMMEDIATE')
    try { const output = fn(); this.db.exec('COMMIT'); return output } catch (error) { this.db.exec('ROLLBACK'); throw error }
  }
  team(team) { return this.db.prepare('SELECT * FROM teams WHERE id=?').get(team) ?? fail('team.not_found', 404) }
  role(team, identity) {
    const member = this.db.prepare('SELECT role,revoked FROM members WHERE team=? AND actor=?').get(team, actorId(identity))
    if (member) return member.revoked ? null : member.role
    const groupRoles = this.configuredTeams.find(t => t.id === team)?.group_roles ?? []
    return ['admin', 'writer', 'reader'].find(role => groupRoles.some(mapping => mapping.role === role && mapping.issuer === identity.issuer && identity.groups?.includes(mapping.group))) ?? null
  }
  admit(team, identity, role = 'reader', feature) {
    const actual = this.role(team, identity)
    if (!roleAtLeast(actual, role)) fail('team.forbidden', 403)
    const row = this.team(team)
    if (feature && !JSON.parse(row.features)[feature]) fail('team.feature_disabled', 403)
    return { row, role: actual }
  }
  teamsFor(identity) { return this.configuredTeams.map(team => ({ tenant_id: team.id, label: team.label, role: this.role(team.id, identity) })).filter(team => team.role) }
  audit(team, actor, action, data = {}) {
    const row = this.team(team)
    const sequence = integer(row.audit_seq + 1, 1)
    const body = canonical({ version: 1, sequence, at: utc(this.clock()), actor_id: actorId(actor), action, data })
    const hash = sha(`${row.audit_hash}\0${body}`)
    this.db.prepare('INSERT INTO audit(team,seq,prev,hash,json) VALUES (?,?,?,?,?)').run(team, sequence, row.audit_hash, hash, body)
    this.db.prepare('UPDATE teams SET audit_seq=?,audit_hash=? WHERE id=?').run(sequence, hash, team)
  }
  mutate(team, identity, method, key, request, fn) {
    id(key); const who = actorId(identity); const hash = sha(jsonBytes(request, 65536))
    return this.transaction(() => {
      const old = this.db.prepare('SELECT hash,result FROM receipts WHERE team=? AND actor=? AND method=? AND key=?').get(team, who, method, key)
      if (old) { if (old.hash !== hash) fail('team.idempotency_conflict', 409); return JSON.parse(old.result) }
      if (this.db.prepare('SELECT COUNT(*) AS n FROM receipts WHERE team=?').get(team).n >= MAX_CHANGES) fail('team.capacity', 507)
      const output = fn()
      this.db.prepare('INSERT INTO receipts(team,actor,method,key,hash,result) VALUES (?,?,?,?,?,?)').run(team, who, method, key, hash, canonical(output))
      return output
    })
  }
  put(team, identity, kind, record, revision, value, tombstone, key) {
    this.admit(team, identity, 'writer', 'metadata_sync'); id(record); integer(revision)
    // A tombstone still names a supported namespace; it cannot create arbitrary key kinds.
    if (!['tags', 'queue_presets', 'workspace_labels'].includes(kind)) fail('team.unsupported_metadata_kind')
    if (!tombstone) metadata(kind, value)
    const body = tombstone ? 'null' : canonical(value)
    return this.mutate(team, identity, 'metadata.put', key, { kind, record, revision, value: tombstone ? null : value, tombstone }, () => {
      this.admit(team, identity, 'writer', 'metadata_sync')
      const tenant = this.team(team)
      const old = this.db.prepare('SELECT * FROM records WHERE team=? AND kind=? AND id=?').get(team, kind, record)
      if ((old?.revision ?? 0) !== revision) fail('team.revision_conflict', 412)
      if (!old && this.db.prepare('SELECT COUNT(*) AS n FROM records WHERE team=?').get(team).n >= MAX_RECORDS) fail('team.capacity', 507)
      if (this.db.prepare('SELECT COUNT(*) AS n FROM changes WHERE team=?').get(team).n >= MAX_CHANGES) fail('team.capacity', 507)
      const bytes = tenant.bytes + Buffer.byteLength(body) - Buffer.byteLength(old?.json ?? '')
      if (bytes > MAX_TENANT_BYTES) fail('team.capacity', 507)
      const seq = integer(tenant.seq + 1, 1); const next = integer(revision + 1, 1)
      this.db.prepare('INSERT INTO records(team,kind,id,revision,seq,deleted,json) VALUES (?,?,?,?,?,?,?) ON CONFLICT(team,kind,id) DO UPDATE SET revision=excluded.revision,seq=excluded.seq,deleted=excluded.deleted,json=excluded.json').run(team, kind, record, next, seq, Number(tombstone), body)
      this.db.prepare('INSERT INTO changes(team,seq,kind,id,revision,deleted,json) VALUES (?,?,?,?,?,?,?)').run(team, seq, kind, record, next, Number(tombstone), body)
      this.db.prepare('UPDATE teams SET seq=?,bytes=? WHERE id=?').run(seq, bytes, team)
      this.audit(team, identity, tombstone ? 'metadata.tombstone' : 'metadata.put', { kind, record_id: record, revision: next, sequence: seq })
      return { kind, record_id: record, revision: next, sequence: seq, tombstone: Boolean(tombstone), value: tombstone ? null : value }
    })
  }
  pull(team, identity, after, snapshot, limit) {
    this.admit(team, identity, 'reader', 'metadata_sync'); integer(after); integer(snapshot); integer(limit, 1, 100)
    const rows = this.db.prepare('SELECT * FROM changes WHERE team=? AND seq>? AND seq<=? ORDER BY seq LIMIT ?').all(team, after, snapshot, limit + 1)
    return { records: rows.slice(0, limit).map(result), more: rows.length > limit, snapshot_sequence: snapshot }
  }
  setFeatures(team, identity, revision, enabled, key) {
    this.admit(team, identity, 'admin'); integer(revision, 1); features(enabled)
    return this.mutate(team, identity, 'policy.features', key, { revision, features: enabled }, () => {
      const row = this.team(team)
      if (row.revision !== revision) fail('team.revision_conflict', 412)
      integer(revision + 1, 1); integer(row.policy_seq + 1, 1)
      this.db.prepare('UPDATE teams SET revision=revision+1,policy_seq=policy_seq+1,features=? WHERE id=?').run(canonical(enabled), team)
      this.audit(team, identity, 'policy.features', { revision: revision + 1 })
      return { revision: revision + 1, sequence: row.policy_seq + 1, features: enabled }
    })
  }
  setMember(team, identity, member, key) {
    this.admit(team, identity, 'admin')
    return this.mutate(team, identity, 'membership.put', key, member, () => {
      this.db.prepare('INSERT INTO members(team,actor,issuer,subject,role,revoked) VALUES (?,?,?,?,?,?) ON CONFLICT(team,actor) DO UPDATE SET role=excluded.role,revoked=excluded.revoked').run(team, actorId(member), member.issuer, member.subject, member.role, Number(member.revoked))
      this.audit(team, identity, 'membership.put', { member_id: actorId(member), role: member.role, revoked: member.revoked })
      return { member_id: actorId(member), role: member.role, revoked: member.revoked }
    })
  }
  receipt(team, identity, method, key) {
    this.admit(team, identity); id(key)
    if (!['metadata.put', 'policy.features', 'membership.put', 'provenance.mint'].includes(method)) fail('team.invalid_receipt_method')
    const row = this.db.prepare('SELECT result FROM receipts WHERE team=? AND actor=? AND method=? AND key=?').get(team, actorId(identity), method, key)
    return row ? JSON.parse(row.result) : fail('team.not_found', 404)
  }
  policy(team, identity, publisher) {
    this.admit(team, identity)
    return this.transaction(() => {
      const row = this.team(team)
      const cached = this.db.prepare('SELECT * FROM policy_leases WHERE team=?').get(team)
      if (cached && cached.seq === row.policy_seq && cached.expires > this.clock() + 60000) return JSON.parse(cached.json)
      const seq = integer(Math.max(row.policy_seq, (cached?.seq ?? 0) + 1), 1)
      const envelope = publisher.policy(team, seq, JSON.parse(row.features), this.clock())
      this.db.prepare('INSERT INTO policy_leases(team,seq,expires,json) VALUES (?,?,?,?) ON CONFLICT(team) DO UPDATE SET seq=excluded.seq,expires=excluded.expires,json=excluded.json').run(team, seq, this.clock() + 900000, canonical(envelope))
      this.db.prepare('UPDATE teams SET policy_seq=? WHERE id=?').run(seq, team)
      this.audit(team, identity, 'policy.lease', { sequence: seq })
      return envelope
    })
  }
  createSession(team, identity, maxTtl = 900000) {
    this.admit(team, identity)
    const token = random(); const expires = Math.min(identity.oidc_expires_at_ms, this.clock() + maxTtl)
    if (expires <= this.clock()) fail('team.sign_in_required', 401)
    this.db.prepare('DELETE FROM sessions WHERE expires<=? OR revoked=1').run(this.clock())
    if (this.db.prepare('SELECT COUNT(*) AS n FROM sessions').get().n >= 10000) fail('team.capacity', 507)
    this.db.prepare('INSERT INTO sessions(hash,team,identity,expires) VALUES (?,?,?,?)').run(sha(token), team, identityBytes(identity), expires)
    return { access_token: token, token_type: 'Bearer', expires_at_ms: expires, tenant_id: team }
  }
  session(token) {
    const row = this.db.prepare('SELECT * FROM sessions WHERE hash=? AND revoked=0 AND expires>?').get(sha(token), this.clock())
    if (!row) fail('team.unauthorized', 401)
    const identity = JSON.parse(row.identity); this.admit(row.team, identity)
    return { tenant: row.team, identity, hash: row.hash, expires_at_ms: row.expires }
  }
  rotateSession(token) {
    return this.transaction(() => {
      const current = this.session(token)
      this.db.prepare('UPDATE sessions SET revoked=1 WHERE hash=?').run(current.hash)
      const next = this.createSession(current.tenant, current.identity)
      this.audit(current.tenant, current.identity, 'session.rotate')
      return next
    })
  }
  revokeSession(token) {
    return this.transaction(() => {
      const current = this.session(token)
      this.db.prepare('UPDATE sessions SET revoked=1 WHERE hash=?').run(current.hash)
      this.audit(current.tenant, current.identity, 'session.revoke')
      return { revoked: true }
    })
  }
  auditView(team, identity, after = 0, limit = 100) {
    this.admit(team, identity, 'admin'); integer(after); integer(limit, 1, 100)
    return this.db.prepare('SELECT seq AS sequence,prev AS previous_hash,hash,json FROM audit WHERE team=? AND seq>? ORDER BY seq LIMIT ?').all(team, after, limit).map(row => ({ sequence: row.sequence, previous_hash: row.previous_hash, hash: row.hash, entry: JSON.parse(row.json) }))
  }
}
