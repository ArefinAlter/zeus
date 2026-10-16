// Windows actual-process smoke harness. Run explicitly with Node after building the package binaries.
// Uses only this repository's debug binaries, new state, and a loopback HTTP fixture. No browser, registry,
// keyring/provider configuration, human approvals, installer registrations, or public network calls.
import assert from 'node:assert/strict';
import { createHash, randomUUID } from 'node:crypto';
import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

assert.equal(process.platform, 'win32', 'Run with Windows Node, not WSL/Linux Node.');
assert.equal(process.argv.length, 2, 'This harness has no path/endpoint overrides.');
// The JavaScript realpath fallback can misparse Rust's extended-length \\?\ drive paths.
// Resolve both result and root through the native Windows implementation before comparing them.
const canonicalExistingPath = (file) => fs.realpathSync.native(file);
const base = canonicalExistingPath(path.join(path.dirname(fileURLToPath(import.meta.url)), '..'));
const binaryDir = canonicalExistingPath(path.join(base, 'target', 'debug'));
const binaries = Object.fromEntries(['zeusd', 'zeus', 'zeus-native-host'].map((name) => {
  const target = path.join(binaryDir, `${name}.exe`);
  assert(!fs.lstatSync(target).isSymbolicLink(), `${name} cannot be a symlink`);
  assert(fs.lstatSync(target).isFile(), `${name} must be a built executable`);
  assert.equal(canonicalExistingPath(target).toLowerCase(), target.toLowerCase());
  return [name, target];
}));
const sha256 = (bytes) => createHash('sha256').update(bytes).digest('hex');
const binaryHashes = Object.fromEntries(Object.entries(binaries).map(([name, file]) => [name, sha256(fs.readFileSync(file))]));
const parent = path.join(base, 'target', 'smoke-runs');
fs.mkdirSync(parent, { recursive: true });
assert(!fs.lstatSync(parent).isSymbolicLink(), 'Smoke root cannot be a reparse/symlink path.');
assert.equal(canonicalExistingPath(parent).toLowerCase(), parent.toLowerCase());
const nonce = randomUUID();
const root = path.join(parent, `daemon-${nonce}`);
fs.mkdirSync(root); // Exclusive: an existing directory is an error, never reused.
const marker = path.join(root, 'owned-by-harness.json');
const ownership = JSON.stringify({ nonce, parentPid: process.pid, binaryDir });
fs.writeFileSync(marker, ownership, { flag: 'wx' });
const assertions = [];
const children = new Set();
let daemon;
let activeInstance;
let server;
let requestCount = 0;
let commandNumber = 0;
let failure;
let crashEvidence;
let transferGeneration = 0;
const slowRanges = [];
const reportPath = path.join(root, 'report.json');
const pause = (ms) => new Promise((resolve) => setTimeout(resolve, ms));
const record = (message) => { assertions.push(message); process.stdout.write(`PASS ${message}\n`); };
const exists = (file) => fs.existsSync(file);
const within = (file, directory) => {
  const relative = path.relative(directory, file);
  return relative !== '' && !relative.startsWith('..') && !path.isAbsolute(relative);
};

function assertOwned(directory = root) {
  assert.equal(fs.readFileSync(marker, 'utf8'), ownership, 'Harness ownership marker changed.');
  assert(!fs.lstatSync(root).isSymbolicLink(), 'Harness directory was replaced.');
  assert.equal(canonicalExistingPath(root).toLowerCase(), root.toLowerCase());
  assert(directory === root || within(directory, root), 'Operation escaped synthetic root.');
  if (exists(directory)) {
    assert(!fs.lstatSync(directory).isSymbolicLink(), 'Synthetic directory was replaced.');
    assert.equal(canonicalExistingPath(directory).toLowerCase(), directory.toLowerCase());
  }
}

function syntheticPaths(name) {
  const directory = path.join(root, name);
  const paths = Object.fromEntries(['state', 'config', 'runtime', 'downloads'].map((kind) => [kind, path.join(directory, kind)]));
  fs.mkdirSync(directory);
  for (const folder of Object.values(paths)) { fs.mkdirSync(folder); assertOwned(folder); }
  const config = `write_roots = [${JSON.stringify(paths.downloads.replaceAll('\\', '/'))}]\n[scheduler]\nconcurrent_jobs = 1\nconnections_per_job = 1\nconnections_per_host = 1\n[timeouts]\nconnect_s = 2\nread_s = 5\nprobe_s = 5\n`;
  fs.writeFileSync(path.join(paths.config, 'config.toml'), config, { flag: 'wx' });
  return { ...paths, directory, discovery: path.join(paths.state, 'zeusd.json') };
}

function environment(paths) {
  const env = { ...process.env };
  // Do not inherit user Zeus paths/profiles or proxy settings. Never repurpose HOME/USERPROFILE.
  for (const key of Object.keys(env)) {
    if (/^ZEUS_/i.test(key) || /^(HTTP|HTTPS|ALL|NO)_PROXY$/i.test(key)) delete env[key];
  }
  return { ...env, ZEUS_STATE_DIR: paths.state, ZEUS_CONFIG_DIR: paths.config,
    ZEUS_RUNTIME_DIR: paths.runtime, ZEUS_LOG: 'warn', NO_COLOR: '1',
    NO_PROXY: '127.0.0.1,localhost,::1' };
}

function launch(binary, args, paths, label) {
  assertOwned(paths.directory);
  const child = spawn(binary, args, { env: environment(paths), cwd: paths.directory,
    stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true, shell: false });
  const item = { child, stdout: '', stderr: '', closed: false, exit: undefined, error: undefined };
  children.add(item);
  for (const [stream, key] of [[child.stdout, 'stdout'], [child.stderr, 'stderr']]) {
    stream.setEncoding('utf8');
    stream.on('data', (text) => {
      item[key] += text;
      if (item[key].length > 2 * 1024 * 1024) {
        item.error = new Error(`${label} exceeded bounded capture`);
        if (!item.closed) child.kill();
      }
    });
  }
  child.once('error', (error) => { item.error = error; });
  item.done = new Promise((resolve) => child.once('close', (code, signal) => {
    item.closed = true;
    item.exit = code;
    item.signal = signal;
    fs.writeFileSync(path.join(root, `${label}.stdout.log`), item.stdout);
    fs.writeFileSync(path.join(root, `${label}.stderr.log`), item.stderr);
    resolve(item);
  }));
  return item;
}

async function finish(item, timeout = 20_000) {
  const deadline = Date.now() + timeout;
  while (!item.closed && Date.now() < deadline) await pause(25);
  if (!item.closed) {
    item.child.kill(); // Only the handle returned by our own spawn; no name/global PID targeting.
    throw new Error(`Owned child ${item.child.pid} exceeded ${timeout}ms`);
  }
  if (item.error) throw item.error;
  return item;
}

function discovery(paths) {
  assertOwned(paths.state);
  assert(daemon && !daemon.closed && daemon.child.exitCode === null && !daemon.error,
    'Owned daemon is no longer alive; refusing any CLI connection.');
  assert(!fs.lstatSync(paths.discovery).isSymbolicLink(), 'Discovery cannot be a symlink.');
  const value = JSON.parse(fs.readFileSync(paths.discovery, 'utf8'));
  assert.equal(value.pid, daemon.child.pid, 'Discovery PID is not the exact daemon we spawned.');
  assert.match(value.endpoint, /^\\\\\.\\pipe\\zeusd-[a-f0-9]{16}$/);
  assert.deepEqual(value.protocol_versions, [1]);
  assert.equal(typeof value.instance_id, 'string');
  assert(value.instance_id.length > 0);
  if (activeInstance) assert.equal(value.instance_id, activeInstance, 'Daemon instance changed.');
  return value;
}

async function start(paths, label) {
  assert(!exists(paths.discovery), 'No discovery may predate this start.');
  for (const [name, binary] of Object.entries(binaries)) {
    assert.equal(sha256(fs.readFileSync(binary)), binaryHashes[name], 'Build changed while smoke was running.');
  }
  activeInstance = undefined;
  daemon = launch(binaries.zeusd, ['run'], paths, label);
  const deadline = Date.now() + 30_000;
  while (!exists(paths.discovery) && !daemon.closed && Date.now() < deadline) await pause(25);
  if (!exists(paths.discovery)) {
    throw new Error(`Owned daemon failed before discovery; no CLI was connected. It may have collided with an existing per-user pipe. See ${label}.stderr.log.`);
  }
  const value = discovery(paths);
  activeInstance = value.instance_id;
  record(`${label}: own live PID ${value.pid}, fresh discovery and unique instance verified`);
  return value;
}

async function cli(paths, args, { timeout = 20_000, allowDaemonExit = false } = {}) {
  discovery(paths); // Do not connect until our live child proves it owns the per-user pipe.
  const label = `cli-${String(++commandNumber).padStart(2, '0')}-${args[0]}`;
  const result = await finish(launch(binaries.zeus, ['--json', ...args], paths, label), timeout);
  assert.equal(result.exit, 0, `${label} failed: ${result.stderr.slice(0, 4000)}`);
  if (!allowDaemonExit) discovery(paths);
  return JSON.parse(result.stdout);
}

async function stop(paths, label) {
  const result = await cli(paths, ['daemon', 'stop'], { allowDaemonExit: true });
  assert.equal(result.accepted, true);
  const exit = await finish(daemon, 20_000);
  assert.equal(exit.exit, 0, `${label} daemon exit failed: ${exit.stderr.slice(0, 2000)}`);
  assert(!exists(paths.discovery), 'Clean shutdown did not remove owned discovery.');
  record(`${label}: authenticated shutdown succeeded and removed discovery`);
  activeInstance = undefined;
}

function removeOwnedCrashDiscovery(paths, killed, bytes, previous) {
  assertOwned(paths.state);
  assert(killed.closed, 'Never remove discovery while the owned process is alive.');
  assert.equal(previous.pid, killed.child.pid);
  assert.equal(previous.instance_id, activeInstance);
  assert(!fs.lstatSync(paths.discovery).isSymbolicLink());
  assert(fs.readFileSync(paths.discovery).equals(bytes), 'Stale discovery changed after our crash.');
  const current = JSON.parse(bytes);
  assert.equal(current.pid, killed.child.pid);
  assert.equal(current.instance_id, previous.instance_id);
  fs.writeFileSync(path.join(root, 'crashed-discovery.json'), bytes, { flag: 'wx' });
  fs.unlinkSync(paths.discovery); // Exact fresh record left by our own, fully exited child only.
  activeInstance = undefined;
}

const primary = syntheticPaths('primary');
const collision = syntheticPaths('collision');
const payload = Buffer.from('Zeus isolated loopback download smoke fixture.\n'.repeat(8192), 'utf8');
const expectedSha256 = sha256(payload);
// Production commits are 4 MiB. A 2 MiB object cannot expose committed-but-incomplete progress.
// 8 MiB, in 64 KiB chunks every 30ms (~2 MiB/s), crosses a real commit before completion.
const slowPayload = Buffer.alloc(8 * 1024 * 1024, 'Zeus crash and resume fixture.\n');
const slowSha256 = sha256(slowPayload);
try {
  const init = await finish(launch(binaries.zeusd, ['init'], primary, 'init'));
  assert.equal(init.exit, 0, `init failed: ${init.stderr}`);
  const privateFiles = ['ipc.secret', 'credentials.json', 'trusted-programs.json', 'credentials/cli.token', 'credentials/agent.token', 'credentials/host.token', 'credentials/a2a.token'];
  const initialHashes = new Map(privateFiles.map((relative) => {
    const file = path.join(primary.state, relative);
    assert(fs.lstatSync(file).isFile(), `Missing private state ${relative}`);
    return [relative, sha256(fs.readFileSync(file))];
  }));
  const again = await finish(launch(binaries.zeusd, ['init'], primary, 'init-again'));
  assert.equal(again.exit, 0, `second init failed: ${again.stderr}`);
  for (const [relative, digest] of initialHashes) assert.equal(sha256(fs.readFileSync(path.join(primary.state, relative))), digest);
  record('init is idempotent and preserves synthetic credentials and explicit binary pins');

  const first = await start(primary, 'daemon-first');
  const status = await cli(primary, ['status']);
  assert.equal(status.daemon.instance_id, first.instance_id);
  assert.equal(status.journal_ok, true);
  assert.equal(status.kill_switch_engaged, false);
  assert.equal((await cli(primary, ['ls'])).items.length, 0);
  record('authenticated CLI status matches owned discovery; journal is writable and jobs are empty');

  assert.deepEqual((await cli(primary, ['cloud', 'accounts', 'list'])).accounts, []);
  assert.deepEqual((await cli(primary, ['cloud', 'list'])).deliveries, []);
  record('cloud account and delivery routes return empty isolated state without account or provider access');

  const duplicate = await finish(launch(binaries.zeusd, ['run'], collision, 'daemon-collision'));
  assert.notEqual(duplicate.exit, 0, 'Second per-user listener must fail.');
  assert.match(duplicate.stderr, /bind|listener|pipe|access|unavailable/i, 'Collision failure must identify endpoint failure.');
  assert(!exists(collision.discovery), 'Colliding daemon must not publish discovery.');
  assert(!exists(path.join(collision.state, 'journal.redb')), 'Colliding daemon must fail before opening runtime.');
  discovery(primary);
  record('second daemon with independent state cannot replace the owned first-instance named pipe');

  const route = `/fixture-${nonce}.txt`;
  const slowRoute = `/crash-fixture-${nonce}.txt`;
  server = http.createServer((request, response) => {
    requestCount++;
    assert(['127.0.0.1', '::ffff:127.0.0.1'].includes(request.socket.remoteAddress));
    if (![route, slowRoute].includes(request.url) || !['GET', 'HEAD'].includes(request.method)) {
      response.writeHead(404).end(); return;
    }
    const slow = request.url === slowRoute;
    const content = slow ? slowPayload : payload;
    const digest = slow ? slowSha256 : expectedSha256;
    let start = 0;
    let end = content.length - 1;
    let code = 200;
    const headers = { 'Content-Type': 'text/plain', 'Content-Disposition': `attachment; filename="${slow ? 'crash-fixture.txt' : 'fixture.txt'}"`,
      'Accept-Ranges': 'bytes', ETag: `"${digest}"`, 'Last-Modified': 'Mon, 01 Jan 2024 00:00:00 GMT' };
    if (request.headers.range) {
      const match = /^bytes=(\d+)-(\d*)$/.exec(request.headers.range);
      if (!match) { response.writeHead(416, { 'Content-Range': `bytes */${content.length}` }).end(); return; }
      start = Number(match[1]);
      end = match[2] ? Number(match[2]) : end;
      if (!Number.isSafeInteger(start) || !Number.isSafeInteger(end) || start > end || end >= content.length) {
        response.writeHead(416, { 'Content-Range': `bytes */${content.length}` }).end(); return;
      }
      code = 206;
      headers['Content-Range'] = `bytes ${start}-${end}/${content.length}`;
    }
    if (slow && request.method === 'GET') slowRanges.push({ generation: transferGeneration, start, end, ranged: code === 206, ifRange: request.headers['if-range'] ?? null });
    headers['Content-Length'] = String(end - start + 1);
    response.writeHead(code, headers);
    if (request.method === 'HEAD') { response.end(); return; }
    if (!slow) { response.end(content.subarray(start, end + 1)); return; }
    let offset = start;
    let timer;
    const pump = () => {
      if (response.destroyed || response.writableEnded) return;
      const next = Math.min(offset + 64 * 1024, end + 1);
      const ready = response.write(content.subarray(offset, next));
      offset = next;
      if (offset > end) { response.end(); return; }
      const schedule = () => { timer = setTimeout(pump, 30); };
      if (ready) schedule(); else response.once('drain', schedule);
    };
    response.once('close', () => clearTimeout(timer));
    pump();
  });
  server.on('clientError', (_error, socket) => socket.destroy());
  await new Promise((resolve, reject) => { server.once('error', reject); server.listen(0, '127.0.0.1', resolve); });
  const address = server.address();
  assert.equal(address.address, '127.0.0.1');
  const url = `http://127.0.0.1:${address.port}${route}`;
  const probe = await cli(primary, ['probe', url]);
  assert.equal(probe.probe.total_bytes, payload.length);
  assert.equal(probe.probe.requires_session, false);
  const getArgs = ['get', url, '--output', primary.downloads, '--sha256', expectedSha256,
    '--idempotency-key', `smoke-${nonce}`];
  const enqueued = await cli(primary, getArgs);
  assert.equal(enqueued.jobs.length, 1);
  assert(!enqueued.jobs[0].error, 'Loopback job must be accepted.');
  const jobId = enqueued.jobs[0].job_id;
  assert.match(jobId, /^job_/);
  const waited = await cli(primary, ['wait', jobId, '--timeout', '30'], { timeout: 35_000 });
  assert.equal(waited.jobs[0].state, 'completed');
  const job = (await cli(primary, ['status', jobId])).job;
  assert.equal(job.state, 'completed');
  assert.equal(job.result.sha256, expectedSha256);
  assert.equal(job.result.size_bytes, payload.length);
  const resultPath = canonicalExistingPath(job.result.path);
  assert(within(resultPath, canonicalExistingPath(primary.downloads)), 'Final file escaped synthetic write root.');
  assert(fs.lstatSync(resultPath).isFile(), 'Completed result must resolve to a regular file.');
  assert.equal(sha256(fs.readFileSync(resultPath)), expectedSha256);
  assert(fs.readFileSync(resultPath).equals(payload));
  const verified = await cli(primary, ['verify', jobId, '--sha256', expectedSha256]);
  assert.equal(verified.matches_recorded, true);
  assert.equal(verified.matches_expected, true);
  assert.equal(verified.quarantined, false);
  record('loopback probe, enqueue, wait, exact downloaded bytes and independent CLI verification passed');

  const replay = await cli(primary, getArgs);
  assert.equal(replay.jobs[0].job_id, jobId);
  assert.equal(replay.replayed, true);
  assert.equal((await cli(primary, ['ls'])).items.length, 1);
  assert.equal((await cli(primary, ['audit', 'verify'])).ok, true);
  record('same enqueue key replays the original job and audit verification passes');
  await stop(primary, 'first shutdown');

  const second = await start(primary, 'daemon-restart');
  assert.notEqual(second.instance_id, first.instance_id);
  const recovered = (await cli(primary, ['status', jobId])).job;
  assert.equal(recovered.state, 'completed');
  assert.equal(recovered.result.sha256, expectedSha256);
  const replayAfterRestart = await cli(primary, getArgs);
  assert.equal(replayAfterRestart.jobs[0].job_id, jobId);
  assert.equal(replayAfterRestart.replayed, true);
  assert.equal((await cli(primary, ['ls'])).items.length, 1);
  assert.equal((await cli(primary, ['audit', 'verify'])).ok, true);
  record('restart recovers completed job and durable idempotency without creating a duplicate');

  const slowUrl = `http://127.0.0.1:${address.port}${slowRoute}`;
  const slowGet = ['get', slowUrl, '--output', primary.downloads, '--sha256', slowSha256,
    '--idempotency-key', `crash-${nonce}`];
  const slowEnqueue = await cli(primary, slowGet);
  assert.equal(slowEnqueue.jobs.length, 1);
  assert(!slowEnqueue.jobs[0].error);
  const slowId = slowEnqueue.jobs[0].job_id;
  assert.match(slowId, /^job_/);
  let interrupted;
  const committedDeadline = Date.now() + 30_000;
  while (Date.now() < committedDeadline) {
    const snapshot = (await cli(primary, ['status', slowId], { timeout: 5000 })).job;
    const committed = snapshot.progress.bytes_done;
    if (snapshot.state === 'downloading' && committed > 0 && committed < slowPayload.length) {
      interrupted = snapshot;
      break;
    }
    assert(!['completed', 'failed', 'canceled'].includes(snapshot.state),
      `Slow job became ${snapshot.state} before a committed-but-incomplete snapshot was observed.`);
    await pause(100);
  }
  assert(interrupted, 'No durable committed progress observed within 30 seconds.');
  assert.equal(interrupted.progress.bytes_total, slowPayload.length);
  const previous = discovery(primary);
  const staleBytes = fs.readFileSync(primary.discovery);
  const killed = daemon;
  assert.equal(killed.child.kill('SIGKILL'), true, 'Failed to terminate the exact owned child.');
  const crashExit = await finish(killed, 10_000);
  assert(crashExit.exit !== 0 || crashExit.signal, 'Crash must not be a successful graceful shutdown.');
  assert(exists(primary.discovery), 'Abrupt termination should leave its own discovery behind.');
  crashEvidence = { jobId: slowId, instanceId: previous.instance_id, pid: previous.pid,
    committedBeforeCrash: interrupted.progress.bytes_done, totalBytes: slowPayload.length,
    expectedSha256: slowSha256, exitCode: crashExit.exit, signal: crashExit.signal };
  record(`abrupt owned-child termination after ${interrupted.progress.bytes_done} committed bytes of ${slowPayload.length}`);
  removeOwnedCrashDiscovery(primary, killed, staleBytes, previous);

  transferGeneration = 1;
  const third = await start(primary, 'daemon-after-crash');
  assert.notEqual(third.instance_id, previous.instance_id);
  const resumed = (await cli(primary, ['status', slowId])).job;
  // Verifying reports bytes hashed rather than transfer progress; the range log below independently
  // proves committed transfer bytes were retained even if completion outruns this first status call.
  if (resumed.state !== 'verifying') assert(resumed.progress.bytes_done >= interrupted.progress.bytes_done,
    'Recovery discarded valid durable progress.');
  if (resumed.state === 'paused') await cli(primary, ['resume', slowId]);
  // In-flight jobs normally recover to queued and resume automatically; paused recovery is explicit.
  const crashWait = await cli(primary, ['wait', slowId, '--timeout', '30'], { timeout: 35_000 });
  assert.equal(crashWait.jobs[0].state, 'completed');
  const completed = (await cli(primary, ['status', slowId])).job;
  assert.equal(completed.result.sha256, slowSha256);
  assert.equal(completed.result.size_bytes, slowPayload.length);
  const crashPath = canonicalExistingPath(completed.result.path);
  assert(within(crashPath, canonicalExistingPath(primary.downloads)));
  assert(fs.lstatSync(crashPath).isFile(), 'Recovered result must resolve to a regular file.');
  assert.equal(sha256(fs.readFileSync(crashPath)), slowSha256);
  assert(fs.readFileSync(crashPath).equals(slowPayload));
  const resumedRequests = slowRanges.filter((range) => range.generation === 1 && range.end > range.start);
  assert(resumedRequests.some((range) => range.ranged && range.start >= interrupted.progress.bytes_done),
    'Restart must issue a nonzero Range request beyond already committed bytes.');
  assert(resumedRequests.every((range) => range.ranged && range.start >= interrupted.progress.bytes_done),
    'Restart re-fetched committed file content instead of resuming.');
  assert(resumedRequests.some((range) => range.ifRange === `"${slowSha256}"`),
    'Resume request must bind the original strong ETag.');
  const crashVerify = await cli(primary, ['verify', slowId, '--sha256', slowSha256]);
  assert.equal(crashVerify.matches_recorded, true);
  assert.equal(crashVerify.matches_expected, true);
  assert.equal((await cli(primary, slowGet)).jobs[0].job_id, slowId);
  assert.equal((await cli(primary, ['ls'])).items.length, 2);
  assert.equal((await cli(primary, ['audit', 'verify'])).ok, true);
  crashEvidence = { ...crashEvidence, recoveredInstanceId: third.instance_id, resumedRequests,
    resultSha256: completed.result.sha256 };
  record('abrupt crash recovery resumes nonzero ranges, preserves committed bytes and matches complete SHA256');
  await stop(primary, 'post-crash shutdown');
  assert(requestCount > 0);
} catch (error) {
  failure = error;
  process.exitCode = 1;
} finally {
  // Never run an unguarded daemon-stop command during cleanup. Kill only our still-live spawn handles.
  for (const item of children) if (!item.closed && item.child.exitCode === null) item.child.kill();
  await Promise.allSettled([...children].map((item) => finish(item, 5000)));
  if (server) {
    server.closeAllConnections();
    await new Promise((resolve) => server.close(resolve));
  }
  assertOwned();
  const report = { ok: !failure, root, binaryHashes, assertions, requestCount, crashEvidence, slowRanges,
    error: failure ? String(failure.stack ?? failure) : undefined,
    note: 'Synthetic state is retained for review; no user daemon, browser registration or provider configuration is touched.' };
  fs.writeFileSync(reportPath, JSON.stringify(report, null, 2) + '\n');
  process.stdout.write(`Report: ${reportPath}\n`);
  if (failure) process.stderr.write(`${failure.stack ?? failure}\n`);
}
