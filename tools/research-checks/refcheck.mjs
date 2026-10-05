// Usage: node refcheck.mjs <file.md> [...]
// Extracts arXiv IDs and RFC numbers from markdown, fetches official metadata, prints a table
// with the line where each ID appears so the human/agent can compare titles. Deterministic hallucination guard.
import fs from 'node:fs';

const files = process.argv.slice(2);
if (!files.length) { console.error('usage: node refcheck.mjs file.md ...'); process.exit(2); }

const arxivRe = /(?:arXiv:|arxiv\.org\/(?:abs|pdf)\/)(\d{4}\.\d{4,5})(?:v\d+)?/gi;
const rfcRe = /\bRFC[\s-]?(\d{3,5})\b/gi;
const sleep = (ms) => new Promise(r => setTimeout(r, ms));

const arx = new Map(); // id -> [{file,line,text}]
const rfc = new Map();
for (const f of files) {
  const lines = fs.readFileSync(f, 'utf8').split(/\r?\n/);
  lines.forEach((ln, i) => {
    for (const m of ln.matchAll(arxivRe)) (arx.get(m[1]) ?? arx.set(m[1], []).get(m[1])).push({ f, line: i + 1, text: ln.trim().slice(0, 160) });
    for (const m of ln.matchAll(rfcRe)) (rfc.get(m[1]) ?? rfc.set(m[1], []).get(m[1])).push({ f, line: i + 1, text: ln.trim().slice(0, 160) });
  });
}

async function arxivMeta(ids) {
  const out = new Map();
  for (let i = 0; i < ids.length; i += 40) {
    const batch = ids.slice(i, i + 40);
    const url = `https://export.arxiv.org/api/query?id_list=${batch.join(',')}&max_results=${batch.length}`;
    let xml = '';
    for (let attempt = 0; attempt < 3; attempt++) {
      try { const r = await fetch(url, { headers: { 'User-Agent': 'refcheck/1.0' } }); xml = await r.text(); if (xml.includes('<feed')) break; } catch (e) { xml = ''; }
      await sleep(3500);
    }
    for (const entry of xml.split('<entry>').slice(1)) {
      const id = (entry.match(/<id>https?:\/\/arxiv\.org\/abs\/([^<]+?)(?:v\d+)?<\/id>/) || [])[1];
      const title = ((entry.match(/<title>([\s\S]*?)<\/title>/) || [])[1] || '').replace(/\s+/g, ' ').trim();
      const authors = [...entry.matchAll(/<name>([^<]+)<\/name>/g)].map(m => m[1]);
      const pub = (entry.match(/<published>(\d{4})/) || [])[1];
      if (id) out.set(id, { title, authors, pub });
    }
    await sleep(3500);
  }
  return out;
}

async function rfcMeta(n) {
  try {
    const r = await fetch(`https://www.rfc-editor.org/rfc/rfc${n}.json`, { headers: { 'User-Agent': 'refcheck/1.0' } });
    if (!r.ok) return null;
    const j = await r.json();
    return { title: j.title, status: j.status, pub: j.pub_date, obsoletedBy: (j.obsoleted_by || []).join(','), obsoletes: (j.obsoletes || []).join(',') };
  } catch { return null; }
}

console.log('## arXiv');
const am = await arxivMeta([...arx.keys()]);
for (const [id, refs] of arx) {
  const m = am.get(id);
  console.log(m ? `OK   arXiv:${id} | "${m.title}" | ${m.authors.slice(0,3).join(', ')}${m.authors.length>3?' et al.':''} | ${m.pub}` : `MISS arXiv:${id} | NOT FOUND ON ARXIV`);
  for (const r of refs.slice(0, 2)) console.log(`       ${r.f.split(/[\/]/).pop()}:${r.line}: ${r.text}`);
}
console.log('\n## RFC');
for (const [n, refs] of rfc) {
  const m = await rfcMeta(n);
  console.log(m ? `OK   RFC ${n} | "${m.title}" | ${m.status} | ${m.pub}${m.obsoletedBy ? ' | OBSOLETED BY RFC ' + m.obsoletedBy : ''}${m.obsoletes ? ' | obsoletes ' + m.obsoletes : ''}` : `MISS RFC ${n} | NOT FOUND`);
  for (const r of refs.slice(0, 2)) console.log(`       ${r.f.split(/[\/]/).pop()}:${r.line}: ${r.text}`);
  await sleep(150);
}
