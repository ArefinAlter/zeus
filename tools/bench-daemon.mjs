// Loopback benchmark for the alpha gate (ROADMAP phase 1): idle memory, memory with ten active jobs, and
// throughput at 1, 4 and 16 connections per job. Run explicitly with Node after a release build:
//
//   cargo build --release -p zeus-cli -p zeus-daemon -p zeus-native-host --bins --target-dir target
//   node tools/bench-daemon.mjs
//
// Every daemon runs on new synthetic state under target/bench-runs/<id>, with its own ZEUS_PROFILE so its
// named pipe never collides with a user daemon. The HTTP source is a loopback Node server, so the numbers
// measure the engine, journal and disk path on this machine, not network conditions. No browser, account,
// keyring or public network is used. Only processes this script spawned are ever stopped.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { execFileSync, spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

assert.equal(process.platform, 'win32', 'This harness samples Windows process memory; run it with Windows Node.');
const base = fs.realpathSync.native(path.join(path.dirname(fileURLToPath(import.meta.url)), '..'));
const profileDir = process.argv.includes('--debug') ? 'debug' : 'release';
const binaryDir = path.join(base, 'target', profileDir);
const binaries = Object.fromEntries(['zeusd', 'zeus'].map((name) => {
  const file = path.join(binaryDir, `${name}.exe`);
  assert(fs.lstatSync(file).isFile() && !fs.lstatSync(file).isSymbolicLink(), `build ${file} first`);
  return [name, file];
}));
const id = randomUUID();
// `--root DIR` places the run on another drive (for example an SSD) to separate disk cost from engine cost.
const rootArg = process.argv.indexOf('--root');
const runs = rootArg > 0 ? path.resolve(process.argv[rootArg + 1]) : path.join(base, 'target', 'bench-runs');
const root = path.join(runs, id);
fs.mkdirSync(root, { recursive: true });
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const children = new Set();

function workingSetBytes(pid) {
  // Working set and private bytes of exactly our own child, through the OS process table.
  const out = execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
    `$p = Get-Process -Id ${Number(pid)} -ErrorAction Stop; "$($p.WorkingSet64) $($p.PrivateMemorySize64)"`],
  { encoding: 'utf8', windowsHide: true }).trim().split(/\s+/).map(Number);
  return { working_set_bytes: out[0], private_bytes: out[1] };
}

function diskOf(directory) {
  // The physical disk holding the run: state, journal and downloads all live there.
  try {
    const letter = path.parse(directory).root.slice(0, 1);
    return execFileSync('powershell.exe', ['-NoProfile', '-NonInteractive', '-Command',
      `$d = Get-Partition -DriveLetter ${letter} | Get-Disk; $p = Get-PhysicalDisk | Where-Object FriendlyName -eq $d.FriendlyName | Select-Object -First 1; "$($d.FriendlyName); $($p.MediaType); $($d.BusType)"`],
    { encoding: 'utf8', windowsHide: true }).trim();
  } catch { return 'unknown'; }
}

function launch(binary, args, env, label) {
  const child = spawn(binary, args, { env, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true, shell: false });
  const item = { child, stdout: '', stderr: '', closed: false };
  children.add(item);
  child.stdout.setEncoding('utf8').on('data', (t) => { item.stdout += t; });
  child.stderr.setEncoding('utf8').on('data', (t) => { item.stderr += t; });
  item.done = new Promise((resolve) => child.once('close', (code) => {
    item.closed = true;
    item.exit = code;
    fs.writeFileSync(path.join(root, `${label}.stderr.log`), item.stderr);
    resolve(item);
  }));
  return item;
}

async function finish(item, timeout) {
  const deadline = Date.now() + timeout;
  while (!item.closed && Date.now() < deadline) await pause(20);
  if (!item.closed) { item.child.kill(); throw new Error(`owned child ${item.child.pid} exceeded ${timeout} ms`); }
  return item;
}

function synthetic(name, scheduler) {
  const dir = path.join(root, name);
  const paths = Object.fromEntries(['state', 'config', 'runtime', 'downloads'].map((k) => [k, path.join(dir, k)]));
  for (const p of Object.values(paths)) fs.mkdirSync(p, { recursive: true });
  const config = `write_roots = [${JSON.stringify(paths.downloads.replaceAll('\\', '/'))}]\n[scheduler]\n`
    + `concurrent_jobs = ${scheduler.jobs}\nconnections_per_job = ${scheduler.connections}\n`
    + `connections_per_host = ${Math.min(64, scheduler.connections * scheduler.jobs)}\n`;
  fs.writeFileSync(path.join(paths.config, 'config.toml'), config, { flag: 'wx' });
  const env = { ...process.env };
  for (const key of Object.keys(env)) if (/^ZEUS_/i.test(key) || /^(HTTP|HTTPS|ALL|NO)_PROXY$/i.test(key)) delete env[key];
  Object.assign(env, { ZEUS_STATE_DIR: paths.state, ZEUS_CONFIG_DIR: paths.config, ZEUS_RUNTIME_DIR: paths.runtime,
    ZEUS_PROFILE: `bench-${id.slice(0, 8)}-${name}`, ZEUS_LOG: 'warn', NO_COLOR: '1', NO_PROXY: '127.0.0.1' });
  return { ...paths, env, discovery: path.join(paths.state, 'zeusd.json'), name };
}

async function cli(s, args, timeout = 120_000) {
  const r = await finish(launch(binaries.zeus, ['--json', ...args], s.env, `${s.name}-cli`), timeout);
  assert.equal(r.exit, 0, `zeus ${args.join(' ')} failed: ${r.stderr.slice(0, 2000)}`);
  return JSON.parse(r.stdout);
}

async function start(s) {
  const init = await finish(launch(binaries.zeusd, ['init'], s.env, `${s.name}-init`), 30_000);
  assert.equal(init.exit, 0, init.stderr);
  const daemon = launch(binaries.zeusd, ['run'], s.env, `${s.name}-daemon`);
  const deadline = Date.now() + 30_000;
  while (!fs.existsSync(s.discovery) && !daemon.closed && Date.now() < deadline) await pause(20);
  assert(fs.existsSync(s.discovery), `daemon ${s.name} did not start; see ${s.name}-daemon.stderr.log`);
  assert.equal(JSON.parse(fs.readFileSync(s.discovery, 'utf8')).pid, daemon.child.pid);
  return daemon;
}

async function stop(s, daemon) {
  await cli(s, ['daemon', 'stop']);
  const exit = await finish(daemon, 60_000);
  assert.equal(exit.exit, 0, exit.stderr.slice(0, 2000));
}

// Deterministic payloads served from memory with byte ranges; `paced` throttles each response so ten jobs
// stay active long enough to sample memory under load.
const MiB = 1024 * 1024;
const large = Buffer.alloc(512 * MiB);
for (let i = 0; i < large.length; i += 4096) large.writeUInt32LE((i * 2654435761) >>> 0, i);
const largeSha = createHash('sha256').update(large).digest('hex');
const paced = Buffer.alloc(32 * MiB, 'Zeus paced benchmark payload.\n');
const pacedSha = createHash('sha256').update(paced).digest('hex');
const server = http.createServer((request, response) => {
  const slow = request.url.startsWith('/paced/');
  const content = slow ? paced : large;
  const digest = slow ? pacedSha : largeSha;
  let startAt = 0;
  let end = content.length - 1;
  const headers = { 'Content-Type': 'application/octet-stream', 'Accept-Ranges': 'bytes', ETag: `"${digest}"`,
    'Last-Modified': 'Mon, 01 Jan 2024 00:00:00 GMT' };
  let code = 200;
  const range = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range ?? '');
  if (range) {
    startAt = Number(range[1]);
    end = range[2] ? Math.min(Number(range[2]), end) : end;
    code = 206;
    headers['Content-Range'] = `bytes ${startAt}-${end}/${content.length}`;
  }
  headers['Content-Length'] = String(end - startAt + 1);
  response.writeHead(code, headers);
  if (request.method === 'HEAD') { response.end(); return; }
  if (!slow) { response.end(content.subarray(startAt, end + 1)); return; }
  let offset = startAt;
  let timer;
  const pump = () => {
    if (response.destroyed) return;
    const next = Math.min(offset + 256 * 1024, end + 1);
    const ready = response.write(content.subarray(offset, next));
    offset = next;
    if (offset > end) { response.end(); return; }
    const later = () => { timer = setTimeout(pump, 50); };
    if (ready) later(); else response.once('drain', later);
  };
  response.once('close', () => clearTimeout(timer));
  pump();
});
await new Promise((resolve) => server.listen(0, '127.0.0.1', resolve));
const origin = `http://127.0.0.1:${server.address().port}`;

const report = {
  id, root, build: profileDir,
  machine: { cpu: os.cpus()[0]?.model, logical_cpus: os.cpus().length, memory_bytes: os.totalmem(),
    os: `${os.type()} ${os.release()}`, node: process.version, disk: diskOf(root) },
  conditions: 'Loopback HTTP from a Node server on the same machine; destination on the state drive; no network latency or loss.',
  throughput: [], memory: {},
};
let failure;
try {
  for (const connections of [1, 4, 16]) {
    const s = synthetic(`c${connections}`, { jobs: 1, connections });
    const daemon = await start(s);
    if (connections === 1) {
      await pause(3000);
      report.memory.idle = workingSetBytes(daemon.child.pid);
    }
    const began = process.hrtime.bigint();
    const job = (await cli(s, ['get', `${origin}/large/${connections}.bin`, '--output', s.downloads, '--sha256', largeSha])).jobs[0];
    assert(!job.error, JSON.stringify(job.error));
    const done = await cli(s, ['wait', job.job_id, '--timeout', '600'], 610_000);
    const seconds = Number(process.hrtime.bigint() - began) / 1e9;
    assert.equal(done.jobs[0].state, 'completed', JSON.stringify(done.jobs[0]));
    report.throughput.push({ connections_per_job: connections, bytes: large.length, seconds: Number(seconds.toFixed(3)),
      mib_per_second: Number((large.length / MiB / seconds).toFixed(1)),
      includes: 'probe, ranged transfer, journal commits, SHA-256/BLAKE3 verification and finalization' });
    process.stdout.write(`throughput ${connections} connection(s): ${(large.length / MiB / seconds).toFixed(1)} MiB/s\n`);
    await stop(s, daemon);
  }
  const s = synthetic('ten-jobs', { jobs: 10, connections: 1 });
  const daemon = await start(s);
  const urls = Array.from({ length: 10 }, (_, i) => `${origin}/paced/${i}.bin`);
  const enqueued = await cli(s, ['get', ...urls, '--output', s.downloads]);
  assert.equal(enqueued.jobs.length, 10);
  let peak = { working_set_bytes: 0, private_bytes: 0 };
  let active = 0;
  const deadline = Date.now() + 120_000;
  while (Date.now() < deadline) {
    const list = await cli(s, ['ls', '--limit', '100']);
    active = list.items.filter((j) => j.state === 'downloading').length;
    const sample = workingSetBytes(daemon.child.pid);
    if (active >= 10 && sample.working_set_bytes > peak.working_set_bytes) peak = sample;
    if (list.items.every((j) => ['completed', 'failed', 'canceled'].includes(j.state))) break;
    await pause(500);
  }
  report.memory.ten_active_jobs_peak = { ...peak, sampled_with_all_ten_downloading: peak.working_set_bytes > 0 };
  await stop(s, daemon);
} catch (error) {
  failure = error;
  process.exitCode = 1;
} finally {
  for (const item of children) if (!item.closed) item.child.kill();
  server.closeAllConnections();
  await new Promise((resolve) => server.close(resolve));
  report.ok = !failure;
  if (failure) report.error = String(failure.stack ?? failure);
  fs.writeFileSync(path.join(root, 'report.json'), JSON.stringify(report, null, 2) + '\n');
  process.stdout.write(`Report: ${path.join(root, 'report.json')}\n`);
  if (failure) process.stderr.write(`${failure.stack ?? failure}\n`);
}
