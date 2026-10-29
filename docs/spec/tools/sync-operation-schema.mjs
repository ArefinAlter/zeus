// Refresh the read-only organizer status schema after generating the typed API draft.
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
const root = resolve(dirname(fileURLToPath(import.meta.url)), '../../..');
const read = path => JSON.parse(readFileSync(resolve(root, path), 'utf8'));
const draft = read('crates/zeus-api/schemas/daemon-api.v1.draft.schema.json');
const method = draft.methods['files.operation.get'];
if (!method) throw Error('Regenerate the typed API schema before syncing organizer status');
function inline(node) {
  if (Array.isArray(node)) return node.map(inline);
  if (!node || typeof node !== 'object') return node;
  if (node.$ref?.startsWith('#/$defs/')) return inline(draft.$defs[node.$ref.slice(8)]);
  const result = Object.fromEntries(Object.entries(node).map(([key, value]) => [key, inline(value)]));
  if (/^uint(8|16|32|64)$/.test(result.format ?? '')) {
    const bits = Number(result.format.slice(4)); delete result.format;
    result.minimum = Math.max(result.minimum ?? 0, 0);
    result.maximum = Math.min(result.maximum ?? Number.MAX_SAFE_INTEGER, bits === 64 ? Number.MAX_SAFE_INTEGER : 2 ** bits - 1);
  }
  return result;
}
const path = 'docs/spec/schema/daemon-api.v1.schema.json', api = read(path), name = 'FilesOperationGet';
api.$defs[name + 'Params'] = inline(method.params);
api.$defs[name + 'Result'] = inline(method.result);
api.$defs[name + 'Request'] = { allOf: [{ $ref: '#/$defs/RequestBase' }, { type: 'object', properties: { method: { const: 'files.operation.get' }, params: { $ref: '#/$defs/' + name + 'Params' } }, required: ['method', 'params'] }] };
api.$defs[name + 'Response'] = { allOf: [{ $ref: '#/$defs/SuccessBase' }, { type: 'object', properties: { result: { $ref: '#/$defs/' + name + 'Result' } } }] };
const text = JSON.stringify(api, null, 2) + '\n';
if (process.argv.includes('--check')) {
  if (readFileSync(resolve(root, path), 'utf8').replaceAll('\r\n', '\n') !== text) throw Error('Stale organizer status API schema');
} else writeFileSync(resolve(root, path), text);
