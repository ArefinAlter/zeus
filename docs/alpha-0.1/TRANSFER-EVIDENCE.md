# Adaptive transfer implementation and evidence

28 October 2026. Core sources are the coordinated alpha development checkout; these results are not a released build or an internet speed guarantee.

The engine separates HTTP request ranges from durable disk units. Requests start from the configured 8–32 MiB preference (with smaller file/tail pieces); strongly identified responses can commit complete checksummed units before EOF. The last unit waits for successful response completion. A spare worker can cancel a slow range, wait for its writer to drain, then split only its still-unacknowledged remainder. Previously acknowledged bytes are not re-requested. Weakly identified resources keep conservative commit-sized requests. The browser proxy keeps its 8 MiB request ceiling.

Every actual HTTP request, including redirects, acquires its origin's FIFO budget. 429/503 and supported transport throttle/reset signals shrink future capacity without aborting active responses; returning permits discharge the reduction before replacements are admitted. Recovery adds one connection after a healthy interval, within configured ceilings. The bounded origin cache retains active budgets and can evict idle entries. A separate per-job controller observes throughput and adjusts dispatch and range-size preferences. Session expiry, revocation, epoch fencing, commit policy and final whole-file hashing remain enforced.

Native Windows verification:

- Control 51 tests, engine 34 tests, HTTP 14 tests: **99 passed**. The engine set includes real HTTP request-size/concurrency adaptation, drained slow-tail reassignment with exact final content, shared-host limiting, independent origins, durable resume, changed validators, cancellation, browser-proxy integrity and workflow budget/security cases.
- Strict release Clippy passed for all three crates and all targets with warnings denied.
- An explicitly run manual paced loopback sample transferred the same 64 MiB object for each fixed ceiling; all three final SHA-256 checks passed.

| Connection ceiling | Elapsed | Peak responding streams | HTTP requests including probe |
|---|---:|---:|---:|
|1|16.010s|1|5|
|4|5.247s|4|5|
|16|5.568s|16|17|

Raw results: `D:/zeus-alpha/evidence/core-paced-benchmark.json`. Logs: `D:/zeus-alpha/logs/core-origin-adaptive-final.log`, `core-alpha-clippy.log`, `core-paced-benchmark.log`. This fixture paces body chunks and runs on the Windows development host with its disk/journal costs. It is not a WAN, mobile, HDD, server-corpus or vendor-comparison measurement. Adaptive control is disabled only for the three fixed-ceiling comparison runs; a separate execution test proves that actual requests adapt.

Four connections outperformed sixteen in this sample. A connection ceiling is a limit, not a promised multiplier. Remaining acceptance work includes broad server/auth/refresh corpora, hardware/filesystem failure matrices, and any additional engine changes made after this checkpoint. Mirrors/hedging, filesystem containment and the rest of the roadmap must retain their own implementation/evidence status rather than inheriting this result.

The shared-engine compatibility checkout is isolated at `/tmp/zeus-alpha/ogun-compat`, pinned to current Ogun main `07d346085b4ba922ac221dbfaef7c6eb83b79a43` (tree `0f67893d81171f864a6f842c737d3dafc5a45ec5`). It resolves the alpha sibling zeus-core without changing the owner's Ogun checkout. Compatibility compilation/tests are queued; this pin alone is not a passing result.
