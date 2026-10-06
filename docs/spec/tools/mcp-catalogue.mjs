// Reference derivation of the MCP tool catalogue (spec 06 sections 5.2, 5.3 and 12).
// Builds the `tools` array a Zeus MCP server publishes in tools/list from 06-mcp-tools-v1.md (titles, normative
// descriptions) and schema/mcp-tools.v1.schema.json (inputs, outputs, annotations), then reports its size and the
// catalogue digest `dev.zeus/catalogue_blake3` (BLAKE3 over the JCS form of the array).
//
// Usage: node mcp-catalogue.mjs            size report and digest
//        node mcp-catalogue.mjs --json     the catalogue as JSON on stdout (implementations may embed it)
//        node mcp-catalogue.mjs --check    exit 1 when a budget of 06 section 12 is exceeded
import canonicalize from 'canonicalize';
import { blake3 } from '@noble/hashes/blake3.js';
import { bytesToHex, utf8ToBytes } from '@noble/hashes/utils.js';
import { readFileSync, readdirSync } from 'node:fs';
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
  };
});

const digest = bytesToHex(blake3(utf8ToBytes(canonicalize(tools))));
if (process.argv.includes('--json')) {
  process.stdout.write(JSON.stringify(tools, null, 2) + '\n');
} else {
  const size = (x) => JSON.stringify(x).length;
  const largest = tools.reduce((a, t) => (size(t) > size(a) ? t : a));
  const report = {
    tools: tools.length,
    descriptions_bytes: tools.reduce((n, t) => n + t.description.length, 0),
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
    if (report.tools_array_bytes > 24000) problems.push('tools array above 24,000 bytes');
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
