// Generates the state tables and diagram of 01-domain-model.md from schema/job-state-machine.v1.json.
// Usage: node gen-state-table.mjs [--write]   (without --write: exit 1 if the document is stale)
import { readFileSync, writeFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const specDir = join(here, '..');
export const machinePath = join(specDir, 'schema', 'job-state-machine.v1.json');
const docPath = join(specDir, '01-domain-model.md');

const esc = (s) => String(s).replaceAll('|', '\\|');

export function renderStates(m) {
  const rows = m.states.map((s) => `| \`${s.name}\` | ${s.terminal ? 'yes' : 'no'} | ${esc(s.description)} |`);
  return ['| State | Terminal | Meaning |', '|---|---|---|', ...rows].join('\n');
}

export function renderTransitions(m) {
  const rows = m.transitions.map((t) => {
    const from = t.from.map((f) => (f === null ? '(none)' : `\`${f}\``)).join(', ');
    const actors = t.actors.map((a) => `\`${a}\``).join(', ');
    const events = t.events.map((e) => `\`${e}\``).join(', ');
    return `| ${t.id} | ${from} | \`${t.to}\` | ${esc(t.trigger)} | ${actors} | ${esc(t.guard) || '-'} | ${events} |`;
  });
  return ['| ID | From | To | Trigger | Actor | Guard | Events emitted |', '|---|---|---|---|---|---|---|', ...rows].join('\n');
}

export function renderActors(m) {
  const rows = Object.entries(m.actors).map(([k, v]) => `| \`${k}\` | ${esc(v)} |`);
  return ['| Actor | Who |', '|---|---|', ...rows].join('\n');
}

export function renderDiagram(m) {
  const edges = new Map();
  for (const t of m.transitions) {
    for (const f of t.from) {
      const key = `${f ?? '[*]'}|${t.to}`;
      if (!edges.has(key)) edges.set(key, []);
      edges.get(key).push(t.id);
    }
  }
  const lines = ['stateDiagram-v2'];
  for (const [key, ids] of edges) {
    const [f, to] = key.split('|');
    lines.push(`    ${f} --> ${to}: ${ids.join(' ')}`);
  }
  for (const s of m.states) if (s.terminal) lines.push(`    ${s.name} --> [*]`);
  return '```mermaid\n' + lines.join('\n') + '\n```';
}

/** Structural checks; returns a list of problems. */
export function checkMachine(m, domainStates, eventNames) {
  const problems = [];
  const names = m.states.map((s) => s.name);
  if (JSON.stringify([...names].sort()) !== JSON.stringify([...domainStates].sort())) problems.push('states differ from domain.schema JobState');
  const ids = new Set();
  const out = new Map(names.map((n) => [n, []]));
  const inc = new Map(names.map((n) => [n, []]));
  for (const t of m.transitions) {
    if (ids.has(t.id)) problems.push(`duplicate transition id ${t.id}`);
    ids.add(t.id);
    if (!names.includes(t.to)) problems.push(`${t.id}: unknown to-state ${t.to}`);
    for (const f of t.from) {
      if (f !== null && !names.includes(f)) problems.push(`${t.id}: unknown from-state ${f}`);
      if (f !== null) out.get(f)?.push(t.id);
    }
    inc.get(t.to)?.push(t.id);
    for (const e of t.events) if (!eventNames.includes(e)) problems.push(`${t.id}: event ${e} not in catalogue`);
    for (const a of t.actors) if (!(a in m.actors)) problems.push(`${t.id}: unknown actor ${a}`);
  }
  for (const s of m.states) {
    if (s.terminal && out.get(s.name).length) problems.push(`terminal state ${s.name} has outgoing transitions`);
    if (!s.terminal && !out.get(s.name).length) problems.push(`non-terminal state ${s.name} has no exit`);
    if (!inc.get(s.name).length) problems.push(`state ${s.name} is unreachable`);
  }
  // reachability from creation, and every state can reach a terminal one
  const adj = new Map(names.map((n) => [n, new Set()]));
  const starts = new Set();
  for (const t of m.transitions) for (const f of t.from) (f === null ? starts.add(t.to) : adj.get(f).add(t.to));
  const seen = new Set(starts);
  const stack = [...starts];
  while (stack.length) for (const n of adj.get(stack.pop())) if (!seen.has(n)) (seen.add(n), stack.push(n));
  for (const n of names) if (!seen.has(n)) problems.push(`state ${n} not reachable from creation`);
  const terminals = new Set(m.states.filter((s) => s.terminal).map((s) => s.name));
  for (const n of names) {
    const vis = new Set([n]);
    const st = [n];
    let ok = terminals.has(n);
    while (st.length && !ok) for (const x of adj.get(st.pop())) { if (terminals.has(x)) ok = true; if (!vis.has(x)) (vis.add(x), st.push(x)); }
    if (!ok) problems.push(`state ${n} cannot reach a terminal state`);
  }
  return problems;
}

function splice(text, name, body) {
  const begin = `<!-- BEGIN GENERATED: ${name} -->`;
  const end = `<!-- END GENERATED: ${name} -->`;
  const i = text.indexOf(begin);
  const j = text.indexOf(end);
  if (i < 0 || j < 0) throw new Error(`markers for ${name} missing in 01-domain-model.md`);
  return text.slice(0, i + begin.length) + '\n' + body + '\n' + text.slice(j);
}

export function renderDoc(text, m) {
  let out = text;
  out = splice(out, 'states', renderStates(m));
  out = splice(out, 'actors', renderActors(m));
  out = splice(out, 'transitions', renderTransitions(m));
  out = splice(out, 'diagram', renderDiagram(m));
  return out;
}

if (process.argv[1] && fileURLToPath(import.meta.url) === process.argv[1]) {
  const m = JSON.parse(readFileSync(machinePath, 'utf8'));
  const text = readFileSync(docPath, 'utf8');
  const next = renderDoc(text, m);
  if (process.argv.includes('--write')) {
    writeFileSync(docPath, next);
    console.log('01-domain-model.md updated');
  } else if (next !== text) {
    console.error('01-domain-model.md is stale: run npm run gen:state-table');
    process.exit(1);
  } else console.log('state tables up to date');
}
