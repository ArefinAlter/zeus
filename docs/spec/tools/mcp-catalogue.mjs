// Reference derivation of the MCP tool catalogue (spec 06 sections 5.2, 5.3 and 12).
// Builds the `tools` array a Zeus MCP server publishes in tools/list from 06-mcp-tools-v1.md (titles, normative
// descriptions) and schema/mcp-tools.v1.schema.json (inputs, outputs, annotations), then reports its size and the
// catalogue digest `dev.zeus/catalogue_blake3` (BLAKE3 over the JCS form of the array).
//
// Usage: node mcp-catalogue.mjs            size report and digest
//        node mcp-catalogue.mjs --json     the catalogue as JSON on stdout (implementations may embed it)
//        node mcp-catalogue.mjs --check    exit 1 when a budget of 06 section 12 is exceeded
import canonicalize from 'canonicalize';
import Ajv2020 from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';
import { blake3 } from '@noble/hashes/blake3.js';
import { bytesToHex, utf8ToBytes } from '@noble/hashes/utils.js';
import { readFileSync, readdirSync, mkdirSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const specDir = join(here, '..');
const schemas = Object.fromEntries(
  readdirSync(join(specDir, 'schema'))
    .filter((f) => f.endsWith('.schema.json'))
    .map((f) => JSON.parse(readFileSync(join(specDir, 'schema', f), 'utf8')))
    .map((s) => [s.$id, s]),
);
const MCP = 'https://zeus.invalid/schema/mcp-tools.v1';
const defs = schemas[MCP].$defs;
const doc = readFileSync(join(specDir, '06-mcp-tools-v1.md'), 'utf8');

// Section 5.2: keywords removed from the wire input schema; section 5.3: keywords kept in the wire output schema.
const INPUT_DROP = new Set(['$id', '$schema', 'title', '$defs', 'dependentSchemas', 'if', 'then', 'else', 'not', 'examples', '$comment']);
const OUTPUT_KEEP = new Set(['type', 'properties', 'required', 'items', 'anyOf']);

function resolve(ref, base) {
  const [docId, frag] = ref.startsWith('#') ? [base, ref.slice(1)] : ref.split('#');
  let node = schemas[docId];
  for (const part of frag.split('/').filter(Boolean)) node = node[part];
  return [node, docId];
}

// Inline every $ref; keep `description` only where the mcp-tools definition itself writes it.
function inline(node, base, ownDescriptions) {
  if (Array.isArray(node)) return node.map((x) => inline(x, base, ownDescriptions));
  if (!node || typeof node !== 'object') return node;
  let out = {};
  if (node.$ref) {
    const [target, targetBase] = resolve(node.$ref, base);
    out = { ...inline(target, targetBase, false) };
    delete out.description;
  }
  for (const [key, value] of Object.entries(node)) {
    if (key === '$ref' || INPUT_DROP.has(key)) continue;
    if (key === 'description' && !ownDescriptions) continue;
    if (key === 'enum' || key === 'const' || key === 'default') out[key] = value;
    else if (key === 'properties') out.properties = Object.fromEntries(Object.entries(value).map(([p, s]) => [p, inline(s, base, ownDescriptions)]));
    else out[key] = inline(value, base, ownDescriptions);
  }
  return out;
}

function structureOnly(node) {
  if (Array.isArray(node)) return node.map(structureOnly);
  if (!node || typeof node !== 'object') return node;
  const out = {};
  for (const [key, value] of Object.entries(node)) {
    if (!OUTPUT_KEEP.has(key)) continue;
    out[key] = key === 'properties' ? Object.fromEntries(Object.entries(value).map(([p, s]) => [p, structureOnly(s)])) : structureOnly(value);
  }
  if (out.type === undefined && Array.isArray(node.enum)) out.type = 'string';
  return out;
}

const pascal = (name) => name.replace(/^zeus_/, '').split('_').map((w) => w[0].toUpperCase() + w.slice(1)).join('');
const titleOf = (name) => {
  const m = doc.match(new RegExp('^\\| \\d+ \\| `' + name + '` \\| ([^|]+) \\|', 'm'));
  if (!m) throw new Error(`no title row for ${name} in section 6`);
  return m[1].trim();
};
const descriptionOf = (name) => {
  const m = doc.match(new RegExp('### 6\\.\\d+ `' + name + '`\\n\\n\\*\\*Description \\(normative\\):\\*\\* "([^"]+)"'));
  if (!m) throw new Error(`no normative description for ${name}`);
  return m[1];
};

const tools = defs.ToolName.enum.map((name) => {
  const title = titleOf(name);
  return {
    name,
    title,
    description: descriptionOf(name),
    inputSchema: inline(defs[`${pascal(name)}Input`], MCP, true),
    outputSchema: {
      type: 'object',
      anyOf: [structureOnly(inline(defs[`${pascal(name)}Output`], MCP, false)), structureOnly(inline(defs.ToolError, MCP, false))],
    },
    annotations: { title, ...defs.CatalogueAnnotations.properties[name].const },
    ...(defs.CatalogueUi?.properties[name] ? { _meta: { ui: defs.CatalogueUi.properties[name].const } } : {}),
  };
});

const digest = bytesToHex(blake3(utf8ToBytes(canonicalize(tools))));
const embeddedPath = join(specDir, 'generated', 'mcp-catalogue.json');
if (process.argv.includes('--write')) {
  mkdirSync(dirname(embeddedPath), { recursive: true });
  writeFileSync(embeddedPath, JSON.stringify(tools, null, 2) + '\n');
}
if (process.argv.includes('--json')) {
  process.stdout.write(JSON.stringify(tools, null, 2) + '\n');
} else {
  const size = (x) => Buffer.byteLength(JSON.stringify(x), 'utf8');
  const largest = tools.reduce((a, t) => (size(t) > size(a) ? t : a));
  const report = {
    tools: tools.length,
    descriptions_bytes: tools.reduce((n, t) => n + Buffer.byteLength(t.description, 'utf8'), 0),
    input_schemas_bytes: tools.reduce((n, t) => n + size(t.inputSchema), 0),
    output_schemas_bytes: tools.reduce((n, t) => n + size(t.outputSchema), 0),
    tools_array_bytes: size(tools),
    without_output_schemas_bytes: size(tools.map(({ outputSchema, ...rest }) => rest)),
    largest_tool: `${largest.name} (${size(largest)} bytes)`,
    catalogue_blake3: digest,
  };
  console.log(JSON.stringify(report, null, 2));
  if (process.argv.includes('--check')) {
    const problems = [];
    const expectedNames = ['zeus_probe', 'zeus_enqueue', 'zeus_status', 'zeus_wait', 'zeus_control', 'zeus_verify', 'zeus_session_list', 'zeus_session_request', 'zeus_files_list', 'zeus_cloud', 'zeus_workflow', 'zeus_files', 'zeus_artifacts', 'zeus_inbox'];
    if (JSON.stringify(tools.map(tool => tool.name)) !== JSON.stringify(expectedNames)) problems.push('the original nine tools and appended cloud tool must retain their catalogue order');
    const cloud = tools.find(tool => tool.name === 'zeus_cloud');
    if (!cloud || !cloud.annotations.destructiveHint || cloud.annotations.readOnlyHint || !cloud.annotations.idempotentHint || !cloud.annotations.openWorldHint) problems.push('cloud annotations do not describe its mutation/cancellation surface');
    if (cloud) {
      const ajv = new Ajv2020({ strict: false, allErrors: true });
      addFormats(ajv);
      const validate = ajv.compile(cloud.inputSchema);
      const wrap = request => ({idempotency_key: 'catalogue-check', request});
      for (const request of [
        {action: 'accounts'}, {action: 'list'}, {action: 'get', delivery_id: 'cld_01M48AKW2RDVBCR29C5HZ0ZD6Y'},
        {action: 'control', delivery_id: 'cld_01M48AKW2RDVBCR29C5HZ0ZD6Y', operation: 'cancel'},
        {action: 'submit', plan_id: 'clp_01M48AKW2RDVBCR29C5HZ0ZD6Y', plan_hash: 'a'.repeat(64)},
        {action: 'plan', account_id: 'research', mode: 'staged_upload', source: {kind:'public_url', url:'https://example.org/report.zip', size_bytes:42}, remote_name:'report.zip', max_bytes:42, max_seconds:1800},
      ]) if (!validate(wrap(request))) problems.push(`cloud ${request.action} request rejected: ${ajv.errorsText(validate.errors)}`);
      for (const request of [
        {action:'configure', credential:'secret'}, {action:'revoke'}, {action:'decide'},
        {action:'accounts', config_file:'ambient'}, {action:'control', delivery_id:'cld_example', operation:'delete'},
        {action:'plan', account_id:'research', source:{kind:'public_url',url:'https://example.org/report.zip',size_bytes:42},remote_name:'report.zip',max_bytes:42,max_seconds:60},
      ]) if (validate(wrap(request))) problems.push(`cloud accepted forbidden or incomplete ${request.action} request`);
      if (validate({...wrap({action:'accounts'}), credential:'secret'})) problems.push('cloud accepted unknown top-level credential field');
    }
    if (report.tools_array_bytes > 40960) problems.push('tools array above 40,960 bytes');
    try {
      if (canonicalize(JSON.parse(readFileSync(embeddedPath, 'utf8'))) !== canonicalize(tools))
        problems.push('embedded catalogue is stale; run with --write');
    } catch {
      problems.push('embedded catalogue is missing or invalid; run with --write');
    }
    for (const t of tools) {
      if (size(t) > 4096) problems.push(`${t.name} above 4,096 bytes`);
      if (t.description.length > 320) problems.push(`${t.name} description above 320 characters`);
      if (!/^zeus_[a-z0-9_]{1,59}$/.test(t.name)) problems.push(`${t.name} breaks the name rule`);
    }
    const stated = doc.match(/Reference catalogue digest of this draft: `([0-9a-f]{64})`/)?.[1];
    if (stated !== digest) problems.push(`06 section 12 states digest ${stated ?? '(none)'}, computed ${digest}`);
    for (const p of problems) console.error('FAIL ' + p);
    if (problems.length) process.exit(1);
    console.log('OK');
  }
}
