// Validates every example in docs/spec/*.md against docs/spec/schema/*.json and cross-checks the vocabulary.
// Usage: node validate-spec.mjs
//
// Fence conventions in the markdown:
//   ```json validate=<schema-name>#/$defs/<Def>      one JSON document, must validate
//   ```jsonl validate=<schema-name>#/$defs/<Def>     one document per line; add "chain" to also verify the audit hash chain
//   ```json fragment                                  illustrative excerpt, deliberately not validated
// Any other json/jsonl fence is an error: no unvalidated example may be left behind.
import Ajv2020 from 'ajv/dist/2020.js';
import addFormats from 'ajv-formats';
import canonicalize from 'canonicalize';
import { blake3 } from '@noble/hashes/blake3.js';
import { bytesToHex, concatBytes, hexToBytes, utf8ToBytes } from '@noble/hashes/utils.js';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';
import { checkMachine, machinePath, renderDoc } from './gen-state-table.mjs';
import { redactUrl } from './redact-url.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const specDir = join(here, '..');
const schemaDir = join(specDir, 'schema');
let failures = 0;
const fail = (msg) => { failures++; console.error('FAIL ' + msg); };

// ---------- load schemas ----------
const schemas = readdirSync(schemaDir)
  .filter((f) => f.endsWith('.schema.json'))
  .map((f) => ({ file: f, json: JSON.parse(readFileSync(join(schemaDir, f), 'utf8')) }));

const refKey = (base, ref) => (ref.startsWith('#') ? base + ref : ref);
// $defs that are used as allOf members must not receive unevaluatedProperties:false themselves.
const allOfTargets = new Set();
for (const { json } of schemas) {
  (function walk(n) {
    if (Array.isArray(n)) return n.forEach(walk);
    if (n && typeof n === 'object') {
      if (Array.isArray(n.allOf)) for (const m of n.allOf) if (m.$ref) allOfTargets.add(refKey(json.$id, m.$ref));
      for (const v of Object.values(n)) walk(v);
    }
  })(json);
}

function strictify(json) {
  const clone = structuredClone(json);
  (function walk(n, path, inAllOf) {
    if (Array.isArray(n)) return n.forEach((x, i) => walk(x, path + '/' + i, path.endsWith('/allOf')));
    if (!n || typeof n !== 'object') return;
    const defName = path.match(/^\/\$defs\/([^/]+)$/)?.[1];
    const skip = inAllOf || (defName && allOfTargets.has(`${json.$id}#/$defs/${defName}`));
    const isSchemaWithProps = n.properties && typeof n.properties === 'object' && (n.type === 'object' || n.type === undefined);
    if (!skip && (isSchemaWithProps || (Array.isArray(n.allOf) && !n.$ref)) && n.unevaluatedProperties === undefined && n.additionalProperties === undefined) {
      n.unevaluatedProperties = false;
      if (n.type === undefined) n.type = 'object';
    }
    for (const [k, v] of Object.entries(n)) {
      if (k === 'enum' || k === 'const' || k === 'default' || k === 'examples') continue;
      walk(v, path + '/' + k, false);
    }
  })(clone, '', false);
  return clone;
}

function makeAjv(list) {
  const ajv = new Ajv2020({ strict: true, strictRequired: false, allErrors: true, allowUnionTypes: true });
  addFormats(ajv);
  ajv.addKeyword({ keyword: 'x-zeus-secret' });
  for (const s of list) ajv.addSchema(s);
  return ajv;
}
let strictAjv;
try {
  makeAjv(schemas.map((s) => s.json)); // plain compile check of every schema
  strictAjv = makeAjv(schemas.map((s) => strictify(s.json)));
  for (const s of schemas) strictAjv.getSchema(s.json.$id);
} catch (e) {
  console.error('schema compile error:', e.message);
  process.exit(1);
}
const byName = new Map(schemas.map((s) => [s.json.$id.replace('https://zeus.invalid/schema/', ''), s.json.$id]));

function validatorFor(ref) {
  const [name, frag = ''] = ref.split('#');
  const id = byName.get(name);
  if (!id) throw new Error(`unknown schema ${name}`);
  const v = strictAjv.getSchema(id + (frag ? '#' + frag : ''));
  if (!v) throw new Error(`cannot resolve ${ref}`);
  return v;
}

// ---------- audit chain ----------
const GENESIS = 'zeus.audit.genesis.v1';
export function recordHash(prevHashHex, record) {
  const { hash, ...rest } = record;
  return bytesToHex(blake3(concatBytes(hexToBytes(prevHashHex), utf8ToBytes(canonicalize(rest)))));
}
export function genesisHash(chainId) {
  return bytesToHex(blake3(utf8ToBytes(GENESIS + chainId)));
}
function verifyChain(records, where) {
  let prev = null;
  let expectSeq = null;
  for (const r of records) {
    if (prev === null) {
      if (r.action !== 'audit.chain_started') { fail(`${where}: first chain record must be audit.chain_started`); return; }
      prev = genesisHash(r.data.chain_id);
      expectSeq = r.seq;
      if (r.seq !== 1) fail(`${where}: first record seq must be 1`);
    }
    if (r.seq !== expectSeq) fail(`${where}: seq ${r.seq}, expected ${expectSeq}`);
    if (r.prev_hash !== prev) fail(`${where}: seq ${r.seq} prev_hash does not link`);
    const h = recordHash(r.prev_hash, r);
    if (h !== r.hash) fail(`${where}: seq ${r.seq} hash mismatch, expected ${h}`);
    prev = r.hash;
    expectSeq = r.seq + 1;
  }
}

// ---------- markdown examples ----------
let validated = 0;
let fragments = 0;
const mdFiles = readdirSync(specDir).filter((f) => /^\d\d-.*\.md$/.test(f)).sort();
const mdText = new Map();
for (const f of mdFiles) {
  const text = readFileSync(join(specDir, f), 'utf8');
  mdText.set(f, text);
  const fence = /^```(jsonl|json)([^\n]*)\n([\s\S]*?)^```$/gm;
  let m;
  while ((m = fence.exec(text))) {
    const line = text.slice(0, m.index).split('\n').length;
    const where = `${f}:${line}`;
    const info = m[2].trim();
    if (info === 'fragment') { fragments++; continue; }
    const ref = info.match(/validate=(\S+)/)?.[1];
    if (!ref) { fail(`${where}: ${m[1]} block without validate= or fragment`); continue; }
    let v;
    try { v = validatorFor(ref); } catch (e) { fail(`${where}: ${e.message}`); continue; }
    const docs = [];
    try {
      if (m[1] === 'json') docs.push(JSON.parse(m[3]));
      else for (const l of m[3].split('\n')) if (l.trim()) docs.push(JSON.parse(l));
    } catch (e) { fail(`${where}: invalid JSON: ${e.message}`); continue; }
    docs.forEach((d, i) => {
      if (!v(d)) fail(`${where} doc ${i + 1} vs ${ref}:\n  ` + v.errors.slice(0, 6).map((e) => `${e.instancePath || '/'} ${e.message} ${JSON.stringify(e.params)}`).join('\n  '));
      else validated++;
    });
    if (/\bchain\b/.test(info)) verifyChain(docs, where);
  }
}

// ---------- vocabulary cross-checks ----------
const domain = schemas.find((s) => s.json.$id.endsWith('/domain')).json;
const common = schemas.find((s) => s.json.$id.endsWith('/common')).json;
const nm = schemas.find((s) => s.json.$id.endsWith('/native-messaging.v1')).json;
const machine = JSON.parse(readFileSync(machinePath, 'utf8'));
const eventTypes = domain.$defs.Event.oneOf.map((r) => domain.$defs[r.$ref.split('/').pop()].allOf[1].properties.type.const);
for (const p of checkMachine(machine, domain.$defs.JobState.enum, eventTypes)) fail('state machine: ' + p);
if (JSON.stringify([...machine.events].sort()) !== JSON.stringify([...eventTypes].sort())) fail('state machine events differ from domain.schema Event catalogue');

const need = (file, words, label) => {
  const t = mdText.get(file) ?? '';
  for (const w of words) if (!t.includes('`' + w + '`')) fail(`${file}: ${label} \`${w}\` is not documented`);
};
need('01-domain-model.md', domain.$defs.JobState.enum, 'job state');
need('01-domain-model.md', common.$defs.ErrorClass.enum.slice(0, 9), 'job error class');
need('00-conventions.md', common.$defs.ErrorClass.enum, 'error class');
need('01-domain-model.md', eventTypes, 'event');
need('01-domain-model.md', domain.$defs.StateCause.enum, 'state cause');
need('01-domain-model.md', domain.$defs.QuarantineReason.enum, 'quarantine reason');
const nmTypes = Object.keys(nm.$defs).filter((k) => k.endsWith('Message') && !k.includes('DaemonTo') && !k.includes('ExtensionTo') && k !== 'Message')
  .map((k) => nm.$defs[k].allOf[1].properties.type.const);
need('02-native-messaging-v1.md', nmTypes, 'message type');
need('01-domain-model.md', nm.$defs.InterruptReason.enum.filter((x) => x !== 'UNKNOWN'), 'interrupt reason');

// redaction vectors must match the reference implementation
const vectors = JSON.parse(readFileSync(join(schemaDir, 'redaction-vectors.v1.json'), 'utf8'));
for (const c of vectors.cases) {
  const r = redactUrl(c.input, c.mode);
  if (r.url !== c.expected || r.redacted !== c.redacted) fail(`redaction vector "${c.name}" (${c.mode}): got ${JSON.stringify(r)}`);
}

// generated state tables must be fresh
if (renderDoc(mdText.get('01-domain-model.md'), machine) !== mdText.get('01-domain-model.md')) fail('01-domain-model.md: generated state tables are stale (npm run gen:state-table)');


// cross references: "NN section X" must name an existing heading, and relative links must resolve
const headings = new Map();
for (const [f, text] of mdText) {
  const set = new Set();
  for (const m of text.matchAll(/^#{2,4} (\d+(?:\.\d+)?)\.? /gm)) set.add(m[1]);
  headings.set(f.slice(0, 2), set);
}
for (const [f, raw] of mdText) {
  // ignore fenced blocks and inline code, which may contain text that only looks like a link
  const text = raw.replace(/^```[\s\S]*?^```$/gm, '').replace(/`[^`\n]*`/g, '');
  for (const m of text.matchAll(/\b(0\d|1[0-2]) sections? (\d+(?:\.\d+)?)/g)) {
    const set = headings.get(m[1]);
    if (set && !set.has(m[2])) fail(`${f}: reference "${m[0]}" has no such heading`);
  }
  for (const m of text.matchAll(/\]\(([^)#\s]+)(#[^)]*)?\)/g)) {
    if (/^(https?:|mailto:)/.test(m[1])) continue;
    try { readFileSync(join(specDir, m[1])); } catch { fail(`${f}: link target ${m[1]} does not exist`); }
  }
}
for (const [f, raw] of mdText) {
  const text = raw.replace(/^```[\s\S]*?^```$/gm, '').replace(/`[^`\n]*`/g, (c) => (/research/.test(c) ? 'research' : ''));
  const own = headings.get(f.slice(0, 2));
  for (const m of text.matchAll(/\bsections? (\d+(?:\.\d+)*)/g)) {
    const before = text.slice(Math.max(0, m.index - 16), m.index);
    if (/(\d\d|RFC \d+|research|Chrome|MDN|HTTP) $/.test(before) || /(RFC|research)/.test(before.slice(-14))) continue;
    if (!own.has(m[1])) fail(`${f}: local reference "section ${m[1]}" has no such heading`);
  }
}

console.log(`${validated} documents validated, ${fragments} fragments skipped, ${mdFiles.length} spec files scanned`);
if (failures) { console.error(`${failures} failure(s)`); process.exit(1); }
console.log('OK');
