// Explicit offline provisioning. Private key bytes are never printed or overwritten.
import { generateKeyPairSync } from 'node:crypto'
import { existsSync, lstatSync, mkdirSync, writeFileSync } from 'node:fs'
import { resolve, join } from 'node:path'
import { id, publicUrl } from '../src/contracts.mjs'
const [directory, origin, tenant, keyId] = process.argv.slice(2)
try {
  id(tenant); id(keyId)
  const url = publicUrl(origin)
  if (url.origin !== origin || url.pathname !== '/') throw new Error()
  const dir = resolve(directory)
  mkdirSync(dir, { recursive: true, mode: 0o700 })
  if (lstatSync(dir).isSymbolicLink()) throw new Error()
  const keyPath = join(dir, `${keyId}.pem`), pinPath = join(dir, 'pin.json')
  if (existsSync(keyPath) || existsSync(pinPath)) throw new Error()
  const keys = generateKeyPairSync('ed25519')
  const jwk = keys.publicKey.export({ format: 'jwk' })
  const pin = { origin, tenant_id: tenant, keys: { [keyId]: Buffer.from(jwk.x, 'base64url').toString('hex') } }
  writeFileSync(keyPath, keys.privateKey.export({ format: 'pem', type: 'pkcs8' }), { flag: 'wx', mode: 0o600 })
  writeFileSync(pinPath, JSON.stringify(pin, null, 2) + '\n', { flag: 'wx', mode: 0o600 })
  console.log('Created one private signing key and its public pin.json in the requested directory. Distribute only pin.json to desktops through a trusted channel.')
} catch { console.error('Provisioning refused. Use a new directory, valid HTTPS origin, tenant and key ID.'); process.exitCode = 1 }
