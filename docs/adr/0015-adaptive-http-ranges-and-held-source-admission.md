# 0015: Adaptive HTTP ranges and held source admission

Status: implemented in the 0.1 alpha worktree; release acceptance remains separate.
Owner direction: 28 October 2026, IDM-style dynamic transfers with reusable core behavior for Ogun.

The byte commitment unit and the HTTP request size serve different purposes. Keep CRC/journal commitment units stable while allocating larger HTTP ranges lazily for representations with strong validators. Request sizes and concurrency adapt within the configured per-job and per-origin limits. Weakly identified representations retain the existing commitment-unit request contract.

An idle worker can reclaim a sufficiently large unfinished tail. Cancel the old response and await its complete fetch/writer future before publishing replacement ranges. Reuse only acknowledged durable bytes. A disk write is joined before cancellation returns; there are no speculative overlapping writers. Header-latency sampling excludes local semaphore waiting and body duration. It measures time to response headers rather than packet RTT.

Origin budgets cover actual requests, including redirects, rather than only outer jobs. Throttling reduces admission, outstanding permits drain without creating replacement capacity, and recovery is bounded. Retained concurrency targets are hints, not authority to exceed policy or a server limit.

Special sources need to bind their immutable private identity before any worker starts. Add an idempotent held-enqueue operation that persists a paused job in the same journal transaction as admission. It has its own intent domain, cannot alias an ordinary enqueue receipt, and does not accept browser-session authority. Explicit resume remains subject to normal gates. Additive transport hooks have defaults so embedders retain existing behavior unless they opt into the binding.

Windows acceptance currently passes 102 control/engine/HTTP tests and strict Clippy, including durable held admission, restart, concurrent duplicate intake, per-origin budgets, variable request sizes, measured connection changes and deterministic stalled-tail reassignment with exact final bytes. The manual performance fixture is separate. Ogun compatibility, broader daemon integration and release packaging are recorded in the alpha ledger. Filesystem path-race closure remains a separate capability change; this decision does not claim it is solved.
