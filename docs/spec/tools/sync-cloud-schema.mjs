// Reconcile spec 14 with the generated Rust API surface. Run after UPDATE_SCHEMAS=1 cargo test -p zeus-api schema.
// The general API remains independently maintained; only cloud definitions and method envelopes are replaced.
import { readFileSync, writeFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import { dirname, resolve } from 'node:path';

const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const read = path => JSON.parse(readFileSync(resolve(root, path), 'utf8'));
const check = process.argv.includes('--check');
const write = (path, value) => {
  const content = JSON.stringify(value, null, 2) + '\n';
  if (check) {
    if (readFileSync(resolve(root, path), 'utf8').replaceAll('\r\n', '\n') !== content) {
      throw new Error(`stale cloud contract: ${path}; run sync-cloud-schema.mjs`);
    }
  } else writeFileSync(resolve(root, path), content);
};
const draft = read('crates/zeus-api/schemas/daemon-api.v1.draft.schema.json');
const methods = Object.entries(draft.methods).filter(([name]) => name.startsWith('cloud.'));
if (methods.length !== 8) throw new Error('expected all eight cloud daemon methods; regenerate the API schema first');
const selected = new Set();
function collect(node) {
  if (!node || typeof node !== 'object') return;
  if (typeof node.$ref === 'string' && node.$ref.startsWith('#/$defs/')) {
    const name = node.$ref.slice(8);
    if (!selected.has(name)) {
      if (!draft.$defs[name]) throw new Error(`missing generated definition ${name}`);
      selected.add(name);
      collect(draft.$defs[name]);
    }
  }
  Object.values(node).forEach(collect);
}
methods.forEach(([, method]) => { collect(method.params); collect(method.result); });
const defs = Object.fromEntries([...selected].sort().map(name => [name, structuredClone(draft.$defs[name])]));
function normalize(node) {
  if (!node || typeof node !== 'object') return;
  if (/^uint(8|16|32|64)$/.test(node.format ?? '')) {
    const bits = Number(node.format.slice(4));
    node.minimum = Math.max(node.minimum ?? 0, 0);
    node.maximum = Math.min(node.maximum ?? Number.MAX_SAFE_INTEGER, bits === 64 ? Number.MAX_SAFE_INTEGER : 2 ** bits - 1);
    delete node.format;
  }
  Object.values(node).forEach(normalize);
}
normalize(defs);
const ids = { plan_id: '^clp_[0-7][0-9A-HJKMNP-TV-Z]{25}$', delivery_id: '^cld_[0-7][0-9A-HJKMNP-TV-Z]{25}$' };
for (const definition of Object.values(defs)) {
  for (const [name, property] of Object.entries(definition.properties ?? {})) {
    if (ids[name]) Object.assign(property, { type: 'string', pattern: ids[name] });
    if (name === 'account_id') Object.assign(property, { type: 'string', pattern: '^[a-z0-9_-]{1,64}$' });
    if (name === 'idempotency_key') Object.assign(property, { type: 'string', minLength: 1, maxLength: 128 });
    if (name === 'max_seconds') Object.assign(property, { minimum: 1, maximum: 86400 });
    if (name === 'max_bytes' || name === 'max_bytes_per_delivery') property.minimum = 1;
  }
}
Object.assign(defs.CloudListParams.properties.limit, { minimum: 1, maximum: 100 });
Object.assign(defs.CloudAccountsConfigureParams.properties.credential, { 'x-zeus-secret': true, writeOnly: true });
const cloudId = 'https://zeus.invalid/schema/cloud.v1';
write('docs/spec/schema/cloud.v1.schema.json', {
  $schema: 'https://json-schema.org/draft/2020-12/schema', $id: cloudId,
  title: 'Zeus cloud delivery v1',
  description: 'Spec 14 cloud delivery contracts. Derived from the implemented API with normative identifier, budget and secret annotations. Public output never includes private checkpoints.',
  $defs: defs,
});
const api = read('docs/spec/schema/daemon-api.v1.schema.json');
const external = node => JSON.parse(JSON.stringify(node).replaceAll('"#/$defs/', `"${cloudId}#/$defs/`));
for (const [method, shape] of methods) {
  const name = method.split('.').map(word => word[0].toUpperCase() + word.slice(1)).join('');
  api.$defs[name + 'Params'] = external(shape.params);
  api.$defs[name + 'Result'] = external(shape.result);
  api.$defs[name + 'Request'] = { allOf: [
    { $ref: '#/$defs/RequestBase' },
    { type: 'object', properties: { method: { const: method }, params: { $ref: `#/$defs/${name}Params` } }, required: ['method', 'params'] },
  ] };
  api.$defs[name + 'Response'] = { allOf: [
    { $ref: '#/$defs/SuccessBase' },
    { type: 'object', properties: { result: { $ref: `#/$defs/${name}Result` } } },
  ] };
}
write('docs/spec/schema/daemon-api.v1.schema.json', api);
console.log(`${check ? 'Checked' : 'Synchronized'} ${methods.length} cloud methods and ${selected.size} definitions.`);
