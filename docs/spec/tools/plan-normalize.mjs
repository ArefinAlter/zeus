// Reference implementation of the workflow plan normal form and plan hash (docs/spec/08-workflow-plan-v0.md
// section 4.2). Implementations MUST produce the same normal form and hash for the vectors in that section.
// Usage: node plan-normalize.mjs <plan.json>     prints the normal form and the plan hash
import canonicalize from 'canonicalize';
import { blake3 } from '@noble/hashes/blake3.js';
import { bytesToHex, utf8ToBytes } from '@noble/hashes/utils.js';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const clone = (v) => structuredClone(v);
const withDefaults = (obj, defaults) => ({ ...defaults, ...obj });

function selector(s) {
  if (!s) return s;
  return s.kind === 'regex' ? withDefaults(s, { group: 1 }) : { ...s };
}

function field(name, f) {
  const out = withDefaults(f, { extract: { kind: 'text' }, resolve_url: false, required: false });
  if (out.selector) out.selector = selector(out.selector);
  if (name === 'url') {
    out.resolve_url = true;
    out.required = true;
  }
  return out;
}

function fieldMap(map) {
  const out = {};
  for (const [name, f] of Object.entries(map)) out[name] = field(name, f);
  return out;
}

function pagination(p) {
  if (!p) return p;
  switch (p.strategy) {
    case 'page_param':
      return withDefaults(p, { step: 1 });
    case 'offset_limit':
      return withDefaults(p, { start_offset: 0 });
    case 'next_selector':
      return { ...p, selector: selector(p.selector) };
    case 'date_range': {
      const out = withDefaults(p, { date_format: 'iso_date', order: 'ascending' });
      if (out.inner) out.inner = pagination(out.inner);
      return out;
    }
    default:
      return { ...p };
  }
}

function filter(f) {
  switch (f.type) {
    case 'url_regex':
    case 'field_regex':
      return withDefaults(f, { invert: false });
    case 'mime':
      return withDefaults(f, { unknown: 'keep' });
    default:
      return { ...f };
  }
}

/** Returns the normal form of a plan that already passed schema validation. Does not modify its input. */
export function normalizePlan(input) {
  const plan = clone(input);
  if (plan.session) plan.session = withDefaults(plan.session, { scope: 'workflow' });
  if (plan.pagination) plan.pagination = pagination(plan.pagination);
  plan.extract = { ...plan.extract, items: selector(plan.extract.items), fields: fieldMap(plan.extract.fields) };
  if (plan.extract.follow) {
    plan.extract.follow = { ...plan.extract.follow, fields: fieldMap(plan.extract.follow.fields) };
  }
  if (plan.filters) plan.filters = plan.filters.map(filter);
  plan.fetch = withDefaults(plan.fetch, { max_connections_per_file: 1, respect_robots: true, priority: 'low' });
  plan.post = plan.post ? { ...plan.post } : {};
  if (plan.post.convert) plan.post.convert = plan.post.convert.map((c) => withDefaults(c, { keep_original: true }));
  plan.sinks = withDefaults(plan.sinks, { on_existing: 'skip' });
  plan.stop = withDefaults(plan.stop ?? {}, { no_new_items_pages: 3, stop_on_duplicate_page: true, on_drift: 'pause' });
  if (plan.stop.count_hint) plan.stop.count_hint = selector(plan.stop.count_hint);
  return plan;
}

/** Lowercase hex BLAKE3 of the JCS (RFC 8785) form of the normal form. */
export function planHash(input) {
  return bytesToHex(blake3(utf8ToBytes(canonicalize(normalizePlan(input)))));
}

if (process.argv[1] === fileURLToPath(import.meta.url) && process.argv[2]) {
  const plan = JSON.parse(readFileSync(process.argv[2], 'utf8'));
  console.log(JSON.stringify(normalizePlan(plan), null, 2));
  console.log('plan_hash ' + planHash(plan));
}
