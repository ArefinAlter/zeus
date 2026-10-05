// Usage: node mdcheck.mjs <file.md> ...   -> structural lint for research docs
import fs from 'node:fs';
let bad = 0;
for (const f of process.argv.slice(2)) {
  const txt = fs.readFileSync(f, 'utf8');
  const lines = txt.split(/\r?\n/);
  const issues = [];
  if (!/^---\n[\s\S]*?\n---/.test(txt)) issues.push('missing YAML front matter');
  for (const h of ['Implications for', 'Open questions', 'References']) if (!new RegExp('^#{1,3} .*' + h, 'mi').test(txt)) issues.push('missing section: ' + h);
  let inFence = false, fenceLang = '';
  for (let i = 0; i < lines.length; i++) {
    const ln = lines[i];
    const fm = ln.match(/^\s*```(\w*)/);
    if (fm) { inFence = !inFence; fenceLang = fm[1]; continue; }
    if (inFence) continue;
    if (/^\s*\|.*\|\s*$/.test(ln) && /^\s*\|[\s:|-]+\|\s*$/.test(lines[i + 1] || '')) {
      const cols = (s) => s.replace(/\\|/g, '').replace(/`[^`]*`/g, m => m.replace(/\|/g, '')).trim().replace(/^\||\|$/g, '').split('|').length;
      const n = cols(ln);
      let j = i + 2;
      while (j < lines.length && /^\s*\|.*\|\s*$/.test(lines[j])) { if (cols(lines[j]) !== n) issues.push(`table col mismatch at line ${j + 1} (expected ${n}, got ${cols(lines[j])})`); j++; }
    }
    if (/[\u{1F300}-\u{1FAFF}\u{2600}-\u{27BF}]/u.test(ln)) issues.push(`emoji at line ${i + 1}`);
  }
  if (inFence) issues.push('unclosed code fence');
  const words = txt.split(/\s+/).length;
  console.log(`${issues.length ? 'FAIL' : 'ok  '} ${f.split(/[\/]/).pop()} (${words} words, ${lines.length} lines)`);
  issues.slice(0, 15).forEach(x => console.log('   - ' + x));
  if (issues.length) bad++;
}
process.exit(bad ? 1 : 0);
