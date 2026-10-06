export const meta = {
  name: 'zeus-research-corpus',
  description: 'Research, write and adversarially verify ~30 markdown research docs (HTTP/download standards, math, protocols, papers, agent/AI protocols, security, legal, Rust ecosystem) for the Zeus/Zeus project, then index them',
  phases: [
    { title: 'Research', detail: 'one agent per topic: web research + write the doc' },
    { title: 'Verify', detail: 'adversarial citation / claim / math verification, fixed in place' },
    { title: 'Audit', detail: 'audit the claims in the original research.md' },
    { title: 'Critic', detail: 'completeness critic finds gaps in the corpus' },
    { title: 'Gap-fill', detail: 'write + verify the docs the critic asked for' },
    { title: 'Index', detail: 'README index and consolidated references' },
  ],
}

// Machine profile (see tools/workflows/README.md). Nothing machine-specific is hard-coded: args.root is the folder that
// holds the sibling repos zeus, zeus-core, zeus-extension and ogun (for example 'A:\\' or 'F:\\'). The script refuses to
// run without it, so a run on the wrong machine cannot write into an unrelated drive.
if (!args || !args.root) throw new Error("args.root is required: the folder holding the zeus repos, e.g. 'A:\\\\' (see tools/workflows/README.md)")
const BASE = String(args.root).replace(/[\\/]+$/, '')
const APP = BASE + '\\zeus'
const CORE = BASE + '\\zeus-core'
const EXT = BASE + '\\zeus-extension'
const OGUN = BASE + '\\ogun'
const ROOT = APP + '\\docs\\research'
// Helper-script folder holding refcheck.mjs / mdcheck.mjs. Override with args.sp.
const SP = args.sp || APP + '\\tools\\research-checks'
// Resume controls (see HANDOFF.md P1.0): args.skip = slugs already verified; args.verifyOnly = slugs written but unverified;
// args.skipAudits / args.noCritic = skip those stages.
const LITE = !!(args && args.lite)   // lean mode: shorter docs, fewer tool calls, default effort
const HIGH = LITE ? undefined : 'high'
const PRIORITY = ['browser-session-handoff-and-extension-platform', 'browser-download-internals', 'mcp-protocol-and-tool-design', 'agent-security-prompt-injection-and-mcp-threats', 'math-throughput-concurrency-congestion', 'trust-surface-approvals-and-gui-architecture', 'llm-provider-auth-routing-and-local-models', 'filesystems-and-file-management-semantics', 'a2a-and-agent-interoperability-protocols', 'auth-bot-defenses-and-ethical-fidelity', 'legal-ethical-and-policy-landscape', 'rust-ecosystem-and-architecture-survey', 'agentic-browsers-and-web-agent-research', 'scraping-crawling-and-extraction-research', 'workflow-engine-dsl-and-durable-execution', 'conversion-parsing-and-sandboxed-pipelines', 'provenance-dataset-and-content-governance-standards', 'terminal-orchestration-and-cli-tui-design', 'desktop-os-integration-packaging-distribution', 'testing-verification-and-benchmarking-methodology', 'competitor-and-reference-implementation-teardowns', 'llm-data-mining-pipelines-and-dataset-formats', 'business-model-licensing-and-open-core-strategy']
const prio = (d) => { const i = PRIORITY.indexOf(d.slug); return i < 0 ? 999 : i }
const ONLY = (args && args.only) ? new Set(args.only) : null   // segment control: run just these slugs
const SKIP = new Set((args && args.skip) || [])
const VERIFY_ONLY = new Set((args && args.verifyOnly) || [])

const SUMMARY = {
  type: 'object',
  properties: {
    file: { type: 'string' },
    words: { type: 'number' },
    referenceCount: { type: 'number' },
    arxivIds: { type: 'array', items: { type: 'string' } },
    rfcs: { type: 'array', items: { type: 'string' } },
    keyFindings: { type: 'array', items: { type: 'string' } },
    correctionsToResearchMd: { type: 'array', items: { type: 'string' } },
    gaps: { type: 'array', items: { type: 'string' } },
    unverified: { type: 'array', items: { type: 'string' } },
  },
  required: ['file', 'words', 'keyFindings', 'gaps'],
}

const VERIFY = {
  type: 'object',
  properties: {
    file: { type: 'string' },
    citationsChecked: { type: 'number' },
    errorsFound: {
      type: 'array',
      items: {
        type: 'object',
        properties: { claim: { type: 'string' }, problem: { type: 'string' }, fix: { type: 'string' } },
        required: ['claim', 'problem', 'fix'],
      },
    },
    unverifiedRemaining: { type: 'array', items: { type: 'string' } },
    mathExamplesRecomputed: { type: 'number' },
    structureOk: { type: 'boolean' },
    verdict: { type: 'string', enum: ['pass', 'pass-with-fixes', 'needs-rewrite'] },
    notes: { type: 'string' },
  },
  required: ['file', 'errorsFound', 'verdict'],
}

const COMMON = `
You are a senior systems / protocol / security researcher writing ONE document of a research corpus for an open-source project.

PROJECT CONTEXT
- Name: "Zeus" (research.md calls it "Zeus", its old working name): a free, open-core, Rust, local-first "download + file layer for AI agents". A background daemon (tokio) with a segmented, resumable HTTP transfer engine; a Manifest V3 browser extension that hands live logged-in sessions and failed browser downloads to the daemon over native messaging (with an in-browser fetch fallback); an MCP server (later an A2A endpoint); a CLI/TUI; an optional Tauri GUI; BYOK plus subscription-friendly LLM provider ladder; scraping/workflow runner with pagination; sandboxed conversion pipelines; rules/dedupe/file organization; provenance manifests and a hash-chained audit log; human-only approval surfaces. It is NOT an IDM clone. Positioning: the session-aware downloader and file layer for agents.
- The existing market/product research is ${APP}\\research.md. Read it first (at least sections 4, 5 and 8). Do not duplicate it; go far deeper on your assigned topic. Where evidence contradicts research.md, say so in a "Corrections to research.md" section.
- Today's date is 2026-10-05. Your training knowledge may be stale; everything dated 2025 or 2026 MUST be verified on the web. Do not assume a product, spec version or policy is still as you remember it.

HOW TO WORK
- WebSearch and WebFetch are deferred tools: first call ToolSearch with query "select:WebSearch,WebFetch" to load them.
- Prefer primary sources: RFC Editor / IETF datatracker, W3C / WHATWG specs, Chromium / Mozilla / WebKit source and docs, arXiv, ACM / USENIX / IEEE pages, vendor docs, GitHub repos / issues / releases (the GitHub REST API works unauthenticated through PowerShell Invoke-WebRequest or curl for stars, release dates, issue state), crates.io API (send a User-Agent header), court opinions / official gazettes for legal items.
- Deterministic citation checks (USE THEM before finishing): node "${SP}\\refcheck.mjs" <your file> prints the official title / authors / year for every arXiv ID and RFC number in your doc and flags obsolete RFCs. node "${SP}\\mdcheck.mjs" <your file> lints structure and tables. For arithmetic in worked examples compute with node -e and paste the real numbers.
- NEVER cite an arXiv ID, RFC number, CVE, court citation, version number, date or statistic you have not seen in a fetched source or verified with the scripts in this session. If you cannot verify it, drop it or tag it [unverified]. A fabricated citation is the worst possible failure of this task. Seed leads given in the scope list are from memory and may be wrong: verify each (by arXiv API / search by title) and fix the ID or discard it.
- Search for 2025-2026 work you do not know about: recent arXiv papers, recent RFCs / drafts, new spec versions, new CVEs, recent court decisions. Include what you find.
- Paraphrase. Quote at most 25 words per source, attributed. Never paste long passages.

OUTPUT FILE
- Write exactly one file with the Write tool at the absolute path given below. Scratch files only in ${SP}. Do not run git. Do not touch other repo files.
- Format (GitHub-flavored markdown):
  1. YAML front matter: title, slug, status: draft, last_researched: 2026-10-05, confidence (high|medium|low overall), related (list of other doc slugs).
  2. "## TL;DR": 6-10 bullets with the conclusions that matter for building Zeus.
  3. Body sections covering every item of your scope list. Use tables for comparisons, numbered lists for algorithms, mermaid diagrams where they clarify mechanisms. Formulas in GitHub LaTeX ($...$ and $$...$$) with a plain-text restatement of key results; every worked example computed and checked.
  4. "## Implications for Zeus": concrete, decisive design recommendations (what to do, what not to do, defaults, thresholds), each tied to the evidence it rests on, plus candidate tests/benchmarks that would validate it.
  5. "## Corrections to research.md" (only when applicable).
  6. "## Open questions".
  7. "## References": every cited source with a stable key such as [RFC 9110], [arXiv:2307.13854], [Chromium: download design], [Web: short title], with full title, authors/org, year and URL. Cite inline with those keys.
- Style: dense, specific, technical; no marketing, no emoji, no filler. Label *Inference* explicitly where you are inferring. Give dates/versions for anything time-sensitive. Tag contested claims with confidence (High/Medium/Low).
- Length: ${LITE ? 'LEAN MODE. Target 3,500-5,500 words (hard cap 6,500). Budget about 45 tool calls of research, then write; do not exhaustively mine every source. Prioritise the scope items that matter most for building Zeus and cover the rest briefly. Depth and correctness over breadth.' : 'as long as needed to be a genuinely useful engineering and research reference, typically 5,000-10,000 words. Depth and correctness over breadth, but cover every scope item.'}

FINAL MESSAGE
- Reply with the required structured summary only.
`

const DOCS = [
{ slug: 'http-download-semantics', file: '01-http-download-semantics.md', title: 'HTTP semantics for reliable downloads',
scope: `
- RFC 9110 range requests: Range, Accept-Ranges, Content-Range, 206/416, multipart/byteranges, If-Range and strong vs weak validators; conditional requests; representation vs transfer encoding; Content-Encoding and ranges (why identity); Content-Length vs chunked; obsolete RFC 7233/7230-7235 mapping to 9110-9112.
- Probing: HEAD vs ranged GET (Range: bytes=0-0), servers that lie or differ (CDNs, nginx, Apache, IIS, S3, GCS, Azure, Cloudflare, Google Drive), 200-instead-of-206, malformed Content-Range, bytes */size, off-by-one pitfalls. Deliver a concrete probe algorithm (mermaid state machine) and a table of observed real-world behaviours WITH sources.
- Content-Disposition (RFC 6266 + RFC 8187, filename vs filename*), browser divergences and WHATWG behaviour; filename extraction priority order (header, URL path, redirects) and sanitisation inputs.
- Redirects (301/302/303/307/308), cross-origin redirect credential stripping per Fetch spec, redirect chains with signed URLs.
- Signed / expiring URLs: S3 SigV4 presign, GCS signed URLs, Azure SAS, CloudFront signed URLs/cookies: expiry semantics, whether in-flight requests survive expiry, ETag stability, how resume interacts.
- Throttling and errors: 429, 503, Retry-After (delta vs date), RFC 9457 problem details, 416 on resume after file change, 412 on If-Match.
- Caching relevance (RFC 9111, immutable RFC 8246). Digest fields: RFC 9530 Content-Digest / Repr-Digest / Want-*, legacy RFC 3230 Digest, x-amz-checksum-*, GCS crc32c/md5, Azure Content-MD5, S3 multipart ETag semantics.
- Link header RFC 8288 (rel=next pagination). HTTP auth schemes in downloads (Basic, Bearer, Digest RFC 7616). Cookies RFC 6265 / 6265bis (SameSite, Secure, __Host-/__Secure- prefixes, Partitioned/CHIPS) and what matters when replaying cookies from a non-browser client.
- Metalink: RFC 5854 (Metalink/XML) and RFC 6249 (Metalink/HTTP). RFC 8673 (HTTP Random Access and Live Content). IETF HTTP resumable uploads draft as a symmetrical design reference. HTTP/1.1 connection reuse and pipelining. Trailers.
- Output: a normative "Zeus HTTP engine behaviour checklist" (what to send, what to accept, what to treat as fatal, retry classes per status code).` },

{ slug: 'transport-http2-http3-quic-congestion', file: '02-transport-http2-http3-quic-congestion.md', title: 'Transport for bulk transfer: HTTP/2, HTTP/3, QUIC and congestion control',
scope: `
- HTTP/1.1 vs HTTP/2 (RFC 9113) vs HTTP/3 (RFC 9114) for bulk downloads; QUIC RFC 9000/9001/9002; TCP congestion control: Reno (RFC 5681), CUBIC (RFC 9438), BBR (IETF draft; BBRv1/v2/v3 status), RTO RFC 6298, window scaling RFC 7323, receive-window autotuning on Windows/Linux/macOS, ECN and L4S (RFC 9330-9332).
- HTTP/2 flow control (stream vs connection windows, SETTINGS_INITIAL_WINDOW_SIZE, MAX_CONCURRENT_STREAMS), TCP head-of-line blocking, what per-connection throttles mean for multiplexed streams; HTTP/2 priorities (RFC 9218).
- Empirical literature on multi-connection vs multiplexing vs QUIC for bulk transfer (e.g. "Taking a Long Look at QUIC" IMC 2017, "Dissecting Performance of Production QUIC" WWW 2021, "QUIC is not Quick Enough over Fast Internet" and similar; verify IDs/venues), UDP rate-limiting by networks, QUIC CPU cost, GSO/GRO.
- Parallel TCP fairness: RFC 2914 congestion-control principles, "TCP-friendliness", Briscoe "Flow rate fairness: dismantling a religion", MPTCP (RFC 8684) and coupled congestion control (RFC 6356), ethics/etiquette of opening N connections to one host, browser per-host limits.
- DNS and connection setup: Happy Eyeballs (RFC 8305 and v3 draft), SVCB/HTTPS RR (RFC 9460), DoH (RFC 8484), TLS 1.3 (RFC 8446), session resumption, 0-RTT safety for GET (RFC 8470), connection pooling and keep-alive for many small files.
- Rust stack maturity for each (hyper, h2, quinn, h3, reqwest HTTP/3 status, rustls) verified against crates.io/GitHub today.
- Measurement methodology (netem/tc, Windows tools) and a decision procedure: when do N parallel HTTP/1.1 connections help, when HTTP/2 multiplexing, when HTTP/3. Explain mechanisms; numeric models are in the maths docs.` },

{ slug: 'other-transfer-protocols-and-sources', file: '03-other-transfer-protocols-and-sources.md', title: 'Other transfer protocols and content sources',
scope: `
For each protocol give: resume capability, parallelism model, integrity mechanism, auth/session model, Rust crate support (verified), and a Zeus priority (P0/P1/P2/never) in a final matrix.
- FTP (RFC 959, 2428, 3659 REST/MLSD/SIZE), FTPS (RFC 4217), SFTP (draft-ietf-secsh-filexfer, pipelining/performance), SCP deprecation, WebDAV (RFC 4918), rsync algorithm and protocol.
- BitTorrent: BEP 3, 5 (DHT), 9, 10, 11, 14, 19 (web seeds), 29 (uTP), 52 (v2 Merkle); piece selection (rarest-first), choking; magnet URIs; hybrid HTTP + P2P; librqbit design. Cohen "Incentives build robustness" and modelling papers.
- Metalink; IPFS (CIDs, bitswap, gateways, range support).
- Cloud object stores: S3 GetObject Range and multipart, checksums, requester-pays, presign; GCS; Azure Blob; R2; B2; the AWS CRT / s3transfer parallel ranged GET design and its part-size heuristics; gcloud/gsutil sliced downloads; azcopy; Rust object_store and Apache OpenDAL.
- Consumer clouds: Google Drive large-file virus-scan confirmation, Dropbox dl=1, OneDrive/SharePoint, Box: download quirks and API routes.
- Git LFS batch API and transfer adapters; OCI Distribution Spec blob pulls and bearer-token challenge flow; Hugging Face Hub downloads (resolve URLs, X-Linked-Etag, Xet CAS protocol, hf_transfer, hf_xet).
- Usenet NNTP (RFC 3977) + yEnc; rclone as multi-backend reference; high-speed science transfer (GridFTP, Aspera FASP, UDT, Globus).
- Streaming: HLS (RFC 8216, LL-HLS), MPEG-DASH (ISO/IEC 23009-1), CMAF, AES-128 / SAMPLE-AES; segment parallelism; muxing with ffmpeg; the DRM boundary (Widevine/PlayReady/FairPlay, EME) and why Zeus must stop there.
- yt-dlp architecture (extractors, format selection, native HLS vs ffmpeg, fragment retries and concurrency, --cookies-from-browser, impersonation, PO tokens), gallery-dl. Where Zeus should delegate vs implement.` },

{ slug: 'math-throughput-concurrency-congestion', file: '04-math-throughput-concurrency-and-congestion.md', title: 'Mathematics of throughput, concurrency and adaptive control',
scope: `
Derive, state conditions of validity, and give worked examples (computed with node):
- Bandwidth-delay product; window-limited throughput; Mathis model (derivation sketch, assumptions) and the Padhye/PFTK model; CUBIC and BBR throughput behaviour (BBR bandwidth and RTprop estimators, Cardwell et al. CACM 2017).
- Aggregate throughput of N parallel flows and fairness; saturation models X(N) = min(link, server cap, N * per-connection rate); empirical parallelism-vs-throughput models in the data-transfer literature (Hacker/Noble/Athey; Lu et al. PSockets; Yildirim et al. optimal parallelism prediction; verify).
- Online concurrency optimisation: AIMD with Chiu-Jain convergence argument; gradient-descent / online-learning transfer tuners (Marlin SC'21, Falcon, DRL transfer tuning; verify IDs); Bayesian optimisation; bandit formulations; control-theory view and stability; handling noisy measurements (slow-start bias, windowing, outlier filters); throttle detection with change-point methods (CUSUM, Page-Hinkley).
- Throughput estimation and ETA: EWMA, harmonic-mean estimators from DASH ABR (e.g. MPC, Festive; verify), Kalman filter derivation, median filters; ETA error metrics.
- Chunk/range size optimisation: per-request latency overhead, efficiency formula, slow-start restarts, server per-request cost, TLS/h2 reuse, AWS multipart guidance.
- Tail latency: work stealing and dynamic splitting, BitTorrent end-game mode, hedged requests (Dean & Barroso "The Tail at Scale"; derive extra-load bound for a p-quantile trigger), speculative execution (LATE).
- Disk-bound regimes: sequential write throughput, IOPS and queue depth, fsync cost; interaction between N segments and disk.
- High-RTT, satellite, mobile and Wi-Fi regimes.
- Deliverables: pseudocode for Zeus's adaptive controller (segment count, split policy, hedge trigger, per-host caps) with default parameters and justification; a discrete-event simulator specification; a validation methodology. At least 8 numeric worked examples.` },

{ slug: 'math-scheduling-reliability-estimation', file: '05-math-scheduling-reliability-and-estimation.md', title: 'Mathematics of scheduling, reliability, bandits and estimation',
scope: `
Derive, state validity conditions, give computed worked examples:
- Job scheduling: SRPT/SJF optimality, WFQ/GPS (Parekh-Gallager), DRR, stride scheduling; max-min fairness by progressive filling; proportional fairness and Kelly's NUM; EDF; aging and priority inversion.
- Per-host politeness: token bucket and GCRA; concurrency limits; Little's Law; M/M/c and Erlang-C for sizing; global vs per-host budgets.
- Crawl scheduling theory: Cho & Garcia-Molina freshness/age and optimal refresh; frontier ordering; Mercator.
- Retry and backoff: exponential backoff with full/equal/decorrelated jitter (Brooker, AWS Architecture Blog; analyse), Ethernet BEB, retry budgets and circuit breakers (Google SRE), retry storms, honouring Retry-After.
- Failure models: expected completion time under Poisson failures for non-resumable vs resumable transfers (RE-DERIVE and check research.md section 8.10 numbers), Young (1974) and Daly (2006) optimal checkpoint interval applied to optimal chunk-commit interval, restart overhead, Weibull failures; availability of multi-mirror downloads.
- Mirror selection bandits: UCB1 (Auer, Cesa-Bianchi, Fischer 2002), Thompson sampling (Chapelle & Li; Agrawal & Goyal), EXP3, sliding-window / discounted variants for non-stationary mirrors, contextual bandits; stated regret bounds (verified against papers).
- Estimation: confidence intervals, heavy tails, streaming quantile sketches (t-digest, DDSketch, HdrHistogram) for p95 hedge triggers; anomaly/change detection (CUSUM, Page-Hinkley, BOCPD).
- LLM-cost math: token budgets, expected spend with caching, routing/cascade cost models (FrugalGPT, RouteLLM; verify IDs).
- Workflow time/size estimation: critical path/PERT with uncertainty bands for plan previews; estimating total items in paginated listings with unknown counts (capture-recapture, Good-Turing) and false-stop probability of 'stop when no new items'.
- Deliverables: algorithms with pseudocode and recommended defaults for Zeus's scheduler, retry policy, mirror selector and ETA/plan estimator.` },

{ slug: 'integrity-chunking-dedup-delta-erasure', file: '06-integrity-chunking-dedup-delta-and-erasure-coding.md', title: 'Integrity, chunking, deduplication, delta encoding, erasure coding and file splitting',
scope: `
- Hash functions: SHA-2, SHA-3, BLAKE2, BLAKE3 (spec, performance, parallelism); incremental hashing during download vs after; non-crypto checksums (CRC32C, xxHash). 
- Verified streaming: Merkle trees (BEP 52, Bao / BLAKE3 outboard encoding, iroh-blobs verified range requests), segment-level verification and proof-size math; S3/GCS/Azure checksum schemes.
- Collision and birthday bounds: do the maths correctly (check research.md section 8.14 which seems numerically inconsistent) and state dedupe safety.
- Content-defined chunking: Rabin fingerprint, Gear, FastCDC (Xia et al. USENIX ATC'16 + the TPDS follow-up), RAM, AE, UltraCDC, normalized chunking; Hugging Face Xet chunking parameters and measured dedupe; restic / borg / casync / zstd:chunked.
- rsync rolling checksum and algorithm; delta encodings (VCDIFF RFC 3284, bsdiff, xdelta3, zstd --patch-from).
- Duplicate detection: size -> prefix hash -> full hash; MinHash/LSH (Broder), SimHash (Charikar; Manku et al.), perceptual image hashes (pHash, dHash, PDQ), audio fingerprints (Chromaprint), embedding-based dedup (SemDeDup).
- Erasure and fountain codes: Reed-Solomon over GF(2^8)/GF(2^16) and SIMD implementations (Leopard; reed-solomon-simd), LT codes, Raptor/RaptorQ (RFC 5053, 6330), Byers et al. digital fountain (SIGCOMM'98), PAR2 spec and PAR3 status. Recompute research.md section 8.13 with the exact binomial.
- File splitting and joining: formats and manifests (split, HJSplit, 7z/zip multi-volume, tar multi-volume, RAR volumes, WebDataset tar shards, Parquet row groups, NNTP segments); a design for 'split into independently verifiable pieces + manifest + optional parity' and reassembly via offset writes; compression interplay (zstd frames and seekable format, bgzip, xz blocks); content-addressable storage; reflinks / copy-on-write (FICLONE, ReFS block cloning, APFS clonefile) for cheap dedupe.
- Worked examples computed with node: Merkle proof sizes, parity overhead and loss probabilities, expected chunk sizes, MinHash band/row threshold curves.` },

{ slug: 'browser-session-handoff-and-extension-platform', file: '07-browser-session-handoff-and-extension-platform.md', title: 'Browser session handoff and the extension platform',
scope: `
- Feasibility and threat analysis of obtaining a logged-in session: Chrome cookie storage and encryption (DPAPI, v10/v11/v20), App-Bound Encryption (Chrome 127; Google security blog) and its bypass ecosystem described at fact level only (no exploit steps) and why Zeus must never read cookie databases; DBSC (W3C WebAppSec spec, Chrome 146 status, registration/refresh protocol and header names, consequences for non-browser clients); Firefox (cookies.sqlite, Total Cookie Protection partitioning); Safari ITP; third-party cookie status in Chrome and CHIPS.
- MV3 capabilities with exact API details: chrome.downloads (events, DownloadItem.state/error/interruptReason list, download options, onDeterminingFilename, resume, search), chrome.cookies (getAll, partitionKey, storeId), chrome.webRequest (what remains), declarativeNetRequest modifyHeaders, chrome.permissions.request + optional_host_permissions (key to per-origin consent), activeTab, nativeMessaging (framing, size limits both directions, host manifest allowed_origins, per-browser/per-OS registration locations, connectNative vs sendNativeMessage), MV3 service-worker lifetime (idle/event limits, what keeps it alive, native-messaging port behaviour), chrome.offscreen, alarms, storage.session.
- Fetch from the extension service worker: credentials, CORS exemption via host_permissions, forbidden headers, Referer/Origin control via DNR, streaming bodies, memory; streaming large bodies to the native host under message-size limits (compare: native messaging chunks vs localhost WebSocket with token and Chrome Private/Local Network Access prompts vs other); measured or documented throughput limits.
- Firefox differences (MV3 event pages, browser.downloads, blocking webRequest retained, firstPartyDomain cookie isolation), Safari Web Extensions (native messaging only to containing app), Edge/Brave/Vivaldi/Opera/Arc differences.
- Store policies: Chrome Web Store (single purpose, limited use, no remote code, permission justification, review for broad host permissions), AMO, Edge Add-ons; extension ID stability and the manifest key.
- CDP / WebDriver BiDi alternatives: Browser.setDownloadBehavior, Page.downloadWillBegin/Progress, Fetch domain, Network.getCookies; Chrome restrictions on --remote-debugging-port with the default profile (verify exact version and behaviour); BiDi download events spec status; Playwright persistent contexts and download model; connecting to the user's existing browser, pros/cons.
- Session refresh on 401/403, expiring URLs (extension refreshes), what the daemon may keep (never persist cookies; zeroize), header-replay fidelity (UA, sec-ch-ua, Accept-Language, Referer, Origin, Sec-Fetch-*) and the line between legitimate fidelity and evasion.
- Deliverable: a recommended handoff architecture with message types, consent model mapped onto optional_host_permissions, and failure/fallback ladder.` },

{ slug: 'browser-download-internals', file: '08-browser-download-internals.md', title: 'How browsers download: internals and failure modes',
scope: `
- Chromium download stack: DownloadManager/DownloadItem, DownloadInterruptReason list (verify exact enum), resumption preconditions (strong ETag/Last-Modified, Accept-Ranges, If-Range), parallel download feature (history, flags, slicing policy), filename determination, .crdownload handling, Safe Browsing / download protection, Content-Disposition and MIME sniffing -> download vs navigation, insecure-download blocking, dangerous file type list, Mark-of-the-Web and Windows Attachment Execution Service.
- Firefox (DownloadCore, .part files, resume logic), Safari/WebKit (WKDownload, resume data), Edge differences. blob: / data: downloads, <a download>, File System Access API, service-worker streamed downloads (StreamSaver pattern).
- Signed-URL expiry mid-download; how each failure appears to users and to chrome.downloads (error strings; verify the exact list: FILE_FAILED, NETWORK_FAILED, SERVER_FORBIDDEN, SERVER_BAD_CONTENT, USER_CANCELED, etc.).
- KEY DELIVERABLE: a precise mapping table from browser interrupt reasons / HTTP statuses / observable signals to Zeus error classes (transient_network, throttled, auth_expired, url_expired, blocked, permanent) with the recovery action for each.
- Recovery design: reuse bytes_received only when validators match; location/format of partial files; take over the partial file vs restart; avoiding double-writing by browser and daemon; cancelling the browser's download after handoff; user-visible handoff UX and dedupe.
- Linux/macOS/Windows specifics.` },

{ slug: 'auth-bot-defenses-and-ethical-fidelity', file: '09-auth-bot-defenses-and-ethical-fidelity.md', title: 'Authentication, bot defenses and the ethical line for fidelity',
scope: `
- How sites protect downloads: cookie sessions, bearer tokens, OAuth 2.0/OIDC (RFC 6749, 7636 PKCE, 8252 native apps, 8628 device grant, 9449 DPoP, 8705 mTLS, 9700 Security BCP), session binding, signed URLs, Referer-based hotlink protection, per-IP and per-account limits.
- Bot management: Cloudflare Bot Management / Turnstile, Akamai, DataDome, HUMAN, Kasada; TLS and HTTP/2 fingerprinting (JA3, JA4 family, Akamai h2 fingerprint); why rustls looks different; impersonation tools (curl-impersonate, utls, rquest/wreq) and the explicit policy decision NOT to ship impersonation; proof-of-origin tokens (YouTube PO tokens), CAPTCHAs.
- Legitimate alternatives for automated clients: Web Bot Auth (IETF webbotauth WG; HTTP Message Signatures RFC 9421; Signature-Agent), Cloudflare verified/signed agents, pay-per-crawl and HTTP 402, x402, Really Simple Licensing; crawler identification best practice (UA with contact URL).
- robots.txt RFC 9309 details (longest-match, 5xx handling, caching, size limit), crawl-delay (non-standard), X-Robots-Tag, noai meta tags, ai.txt.
- Policy: user-as-actor in own session + in-browser fetch fallback vs evasion; recommended policy tiers; error taxonomy and retry rules for blocks (403 with mitigation headers, 429, 503 challenge pages, interstitial detection -> hand back to the human), CAPTCHA policy (no solving, human-in-loop via the user's own tab), rate-limit and politeness defaults.
- Documented incidents of agentic browsers vs bot detection (cross-reference the legal doc), and measured effects where available.` },

{ slug: 'mcp-protocol-and-tool-design', file: '10-mcp-protocol-and-tool-design.md', title: 'Model Context Protocol and MCP tool design',
scope: `
- MCP history and versions (2024-11-05, 2025-03-26, 2025-06-18, 2025-11-25 and every 2026 release; VERIFY the "2026-07-28 stateless spec" claimed in research.md), governance (Agentic AI Foundation / Linux Foundation, December 2025; verify), transports (stdio, Streamable HTTP, SSE deprecation, stateless mode, session IDs, resumability), JSON-RPC base, capability negotiation.
- Primitives: tools (inputSchema/outputSchema, structuredContent, annotations readOnlyHint/destructiveHint/idempotentHint/openWorldHint, titles, icons, resource_link results, isError), resources (subscriptions, templates), prompts, roots, sampling (client support matrix as of 2026-10 for Claude Code, Codex, OpenCode, Cursor, VS Code Copilot, ChatGPT; verify each), elicitation (form and URL modes; why it is NOT a secure approval channel), tasks (async long-running; SEP-1686 status), progress and cancellation, logging, completions, pagination, MCP Apps / ui:// (SEP-1865; client support).
- Authorization: OAuth 2.1 profile, RFC 9728 protected resource metadata, RFC 8414, RFC 7591 dynamic client registration, Client ID Metadata Documents, RFC 8707 resource indicators, token-passthrough prohibition; stdio servers and environment credentials.
- Registry (registry.modelcontextprotocol.io, server.json, namespace verification) and directories (Smithery, PulseMCP, mcp.so, Docker MCP catalog, VS Code gallery).
- Rust: rmcp current version and spec support (macros, transports, auth, tasks), alternatives; inspector and conformance tooling.
- Client behaviours and context cost: tool-count limits, schema size cost, on-demand tool loading / tool search, "code execution with MCP" (Anthropic engineering) and Cloudflare Code Mode; Agent Skills (SKILL.md) as a complement; comparison with OpenAI function calling / Responses tools, Gemini function calling, OpenAPI-derived tools.
- Tool design guidance for long-running jobs (job id + poll + subscribe + tasks), idempotency keys, pagination, output shaping, token-efficient responses, error messages for LLM recovery, destructive annotations, dry-run defaults. Include full JSON schemas for 3 representative Zeus tools.
- Evidence on MCP-vs-CLI tradeoffs for agents (token cost, reliability) with sources.` },

{ slug: 'a2a-and-agent-interoperability-protocols', file: '11-a2a-and-agent-interoperability-protocols.md', title: 'A2A and the agent interoperability protocol landscape',
scope: `
- A2A: origin (Google, April 2025), Linux Foundation (June 2025), spec versions (0.2.x, 0.3, 1.0; verify exact current version and dates), data model (Agent Card at /.well-known/agent-card.json, skills, capabilities, securitySchemes, signed cards), Task lifecycle states, Message/Part/Artifact, streaming (SSE), push notifications, protocol bindings (JSON-RPC, gRPC, HTTP+JSON), extensions, auth (OAuth/OIDC, mTLS, API keys), discovery, multi-tenancy; official and community SDKs including Rust (a2aproject/a2a-rs, others): status verified today.
- MCP vs A2A division of labour; mapping A2A Tasks to MCP Tasks; IBM ACP (merged into A2A; verify), ANP (DID-based), AGNTCY (OASF, Agent Directory, SLIM), NLWeb, AP2, Agentic Commerce Protocol, x402, AGENTS.md, Agent Skills, OpenAI Apps SDK / AgentKit, Microsoft Agent Framework, Amazon AgentCore, Google ADK; Agentic AI Foundation members and projects.
- Surveys and papers (e.g. arXiv 2505.02279 and 2504.16736 candidates; verify) and security analyses of A2A.
- Agent identity and delegation: OAuth token exchange RFC 8693, on-behalf-of flows, SPIFFE/SPIRE, DID/VC, IETF agent-auth drafts (verify).
- Fit for Zeus: Agent Card for Zeus listing skills (fetch, rescue, organise, convert); mapping of download job states to A2A task states (table); artifacts as file references (URI parts) rather than inline bytes; push notifications for long jobs; auth-required mapped to session consent; trust levels for peers; outbound delegation risks. Provide a proposed Agent Card JSON and a state-mapping table.` },

{ slug: 'agent-security-prompt-injection-and-mcp-threats', file: '12-agent-security-prompt-injection-and-mcp-threats.md', title: 'Agent security, prompt injection and MCP threat model',
scope: `
- Threat model for agents with file + network + session powers: indirect prompt injection (Greshake et al.), the "lethal trifecta" (Willison), tool poisoning / rug pull / shadowing (Invariant Labs), confused deputy, excessive agency (OWASP LLM Top 10 2025, OWASP agentic threat work, MITRE ATLAS).
- MCP incidents and CVEs (VERIFY each against NVD/advisories before citing): MCP Inspector CVE-2025-49596, mcp-remote CVE-2025-6514, Anthropic filesystem server path traversal CVE-2025-53109/53110, Git MCP server CVEs, malicious Postmark MCP package, Supabase MCP lethal trifecta, GitHub MCP toxic flow, Asana MCP exposure, Cursor CurXecute/MCPoison, others found in 2026.
- Papers and benchmarks: AgentDojo, InjecAgent, Agent Security Bench, WASP, MCPTox, MCP safety audit, MCP landscape/security surveys; defences: instruction hierarchy, spotlighting, StruQ/SecAlign, CaMeL, the design-patterns-for-securing-LLM-agents paper, Meta's Agents Rule of Two, vendor guidance (Chrome agentic security architecture, Anthropic Claude for Chrome, OpenAI Atlas hardening; verify). Verify every arXiv ID.
- Apply capability/taint tracking to files and URLs: network-sourced content is tainted; delete/move/convert parameters must come from a trusted channel; dual-LLM and plan-then-execute applied to Zeus workflows.
- Human approval security: consent fatigue, trusted UI, agent-supplied text spoofing, approvals that show daemon-computed facts only.
- Localhost attack surface: DNS rebinding, CSRF to localhost, Origin/Host validation (MCP spec requirement), Chrome Private/Local Network Access rollout, the 0.0.0.0 day issue; IPC hardening (named pipe ACLs and first-instance flag, Unix socket permissions, SO_PEERCRED, GetNamedPipeClientProcessId, token files, macaroon/biscuit capabilities); native-messaging host hardening.
- Supply chain: cargo-vet/audit/deny, reproducible builds, SLSA, Sigstore, npm provenance. Archive/parser attacks: zip-slip, zip bombs, symlink races and TOCTOU (openat2 RESOLVE_BENEATH, cap-std). Sandboxing options per OS. Exfiltration paths. Audit-log design and limits of hash chains. Kill switch, budgets, safe defaults.
- Deliverables: a STRIDE-style threat table per Zeus component and a list of testable 'security invariants'.` },

{ slug: 'agentic-browsers-and-web-agent-research', file: '13-agentic-browsers-and-web-agent-research.md', title: 'Agentic browsers and web-agent research: the download last-mile',
scope: `
- Landscape as of 2026-10 (verify status of each, including the research.md claim that ChatGPT Atlas was shut down on 2026-08-09): Perplexity Comet, Atlas, Dia, Arc, Opera Neon, Edge Copilot Mode, Chrome Gemini/auto browse, Brave, Claude for Chrome, OpenAI Operator / ChatGPT agent, Browser Use, Browserbase/Stagehand, Steel, Anchor, Hyperbrowser, Skyvern, Playwright MCP, Chrome DevTools MCP, Anthropic computer use, OpenAI CUA, Gemini computer-use model.
- What each does about downloads and files (evidence from docs/issues), observation spaces (DOM vs accessibility tree vs screenshots) and why accessibility-tree agents cannot 'see' downloads.
- Benchmarks and papers (verify IDs): WebArena, VisualWebArena, Mind2Web and Online-Mind2Web, WebVoyager, WebShop, WorkArena, OSWorld, BrowseComp, AssistantBench, GAIA, WebRL, UI-TARS, others from 2025-2026; focus on tasks involving file download/upload and failure analyses of web agents.
- Source-level look (GitHub) at download handling in Playwright MCP and browser-use: events, output dirs, workspace-root restrictions, issue state today for #154/#953/#1754/#1768 and browser-use #3879/#3721/#3136/#1853.
- Standards for agent-browser interop: WebMCP (W3C community group / navigator.modelContext; verify), NLWeb, llms.txt, browser agent-mode security architectures.
- Formalise the 'last mile' problem (download as a first-class action). Design 'DownloadBench': a failure-corpus benchmark (categories, how to source legally, metrics) Zeus could publish. Where Zeus sits relative to agentic browsers; adapter designs for Playwright MCP / browser-use / Stagehand.` },

{ slug: 'scraping-crawling-and-extraction-research', file: '14-scraping-crawling-and-extraction-research.md', title: 'Scraping, crawling and extraction: classical and LLM-era research',
scope: `
- Classical: wrapper induction (Kushmerick), RoadRunner, DEPTA, VIPS, Boilerpipe, Readability, trafilatura (paper), selector robustness and self-healing.
- LLM-era (verify all IDs): Evaporate (code synthesis for extraction), MarkupLM, ReaderLM-v2 / Jina Reader, Crawl4AI (adaptive crawling), ScrapeGraphAI, Firecrawl extract, WebWalker, HTML-to-markdown pipelines, LLM pagination detection.
- Structured data in pages: JSON-LD/schema.org/microdata/OpenGraph, RSS/Atom, sitemaps (protocol, index files), robots sitemap directive, hidden-API discovery via network capture (XHR/GraphQL, HAR), Next.js __NEXT_DATA__ and hydration payloads.
- Pagination taxonomy with detection heuristics and algorithms: Link header RFC 8288, page parameters, offset/limit, cursor/keyset, infinite scroll, load-more, date-range chunking, facet partitioning to beat result caps, sitemaps as enumerators; API conventions (GitHub Link, Stripe cursors, JSON:API, OData nextLink, Relay connections). Stopping rules (no-new-items, duplicate-page, count hints, capture-recapture estimate).
- A taxonomy of 'how a file download is triggered on the web' (direct link, redirect chain, form POST, JS blob, XHR -> signed URL, websocket, window.open, countdown interstitial, CAPTCHA-gated, email-gated) with Zeus's handling strategy for each.
- URL canonicalisation (RFC 3986 normalisation, tracking-parameter stripping, canonical link), crawl frontier dedupe (Bloom/cuckoo filters, false-positive math), frameworks (Scrapy, Crawlee, Heritrix, StormCrawler, Colly, spider-rs, Katana, texting_robots), WARC.
- Cost model of LLM-drafted selectors (plan once, run many) vs per-page LLM extraction, with dated price assumptions; validation of LLM-drafted plans (schema checks, canary pages, drift detection, rejecting selectors that match zero or too many nodes).
- Deliverable: a workflow plan intermediate representation proposal (feeds the workflow-engine doc).` },

{ slug: 'llm-provider-auth-routing-and-local-models', file: '15-llm-provider-auth-routing-and-local-models.md', title: 'LLM providers, subscription auth, routing and local models',
scope: `
Re-verify EVERYTHING that the product conversation asserted, from primary sources, with dates:
- OpenAI "Sign in with ChatGPT" for third-party apps (the conversation says DevDay Sept 29 / loopback redirect / client id dynamic_agent_client / weekly caps; check DevDay's real date and the docs), Codex CLI OAuth (PKCE, loopback port), codex exec, Codex app-server and MCP.
- Anthropic consumer terms and Claude Code restrictions, April 2026 enforcement, Agent SDK rules, claude -p; Google Gemini CLI OAuth policy, Antigravity bans, deprecation of consumer Gemini Code Assist; xAI/SuperGrok OAuth; OpenCode auth providers and its MCP sampling support; GitHub Copilot third-party/SDK policy; Cursor/Windsurf.
- OpenRouter OAuth PKCE (/auth, /api/v1/auth/keys, loopback callbacks, fees, BYOK, free-model limits) and alternatives (Vercel AI Gateway, Cloudflare AI Gateway, Together, Groq, Fireworks, LiteLLM, Portkey).
- Local inference: Ollama, llama.cpp, LM Studio, vLLM, mistral.rs, candle; OpenAI-compatible endpoints; structured output / grammar support; small local models suited to classification and renaming (current Qwen, Gemma, Phi, SmolLM, Llama, Mistral options; sizes, quantisation, CPU latency) and embedding models (bge, nomic-embed, EmbeddingGemma, e5; ONNX via ort, fastembed).
- Structured output and constrained decoding (provider JSON-schema modes, XGrammar, Outlines, llguidance, GBNF); Rust LLM clients (async-openai, genai, rig, ollama-rs) maturity; MCP sampling client matrix.
- Policy manifest design: signed JSON, per-provider kill switches, ed25519/minisign/TUF, rollback protection, offline fallback. Credential storage: keyring crate, Windows Credential Manager, macOS Keychain, Secret Service/KWallet, headless fallbacks, age-encrypted file; token refresh/rotation; spend caps and accounting; privacy tiers (metadata-only default, local-only mode, redaction); routing (RouteLLM, FrugalGPT, cascades), caching.
- For every provider route output a table row: official? ToS risk? effort? last-verified date + source URL; and a 'what to re-verify before each release' checklist.` },

{ slug: 'filesystems-and-file-management-semantics', file: '16-filesystems-and-file-management-semantics.md', title: 'Filesystem semantics and file management correctness',
scope: `
- Path rules per OS: Windows MAX_PATH, \\\\?\\ prefix and long-path opt-in, reserved names, trailing dots/spaces, forbidden characters, alternate data streams, 8.3 names, per-directory case sensitivity; macOS APFS normalisation and case-insensitivity; Linux bytes-not-text names and name-length limits; Unicode confusables and bidi override (RTLO) attacks; a filename sanitisation + collision algorithm (case-fold collisions, suffix strategies).
- Atomic ops (rename semantics, MoveFileEx/ReplaceFile/POSIX-semantics rename on Windows 10+, cross-volume moves), durability (fsync, FlushFileBuffers, F_FULLFSYNC, directory fsync; Pillai et al. OSDI'14 and later crash-consistency work), the write/fsync/commit/rename protocol for a download journal.
- Preallocation and sparse files (fallocate, SetEndOfFile/SetFileValidData and its privilege/security implications, FSCTL_SET_SPARSE, F_PREALLOCATE), concurrent positional writes (pwrite, overlapped I/O, seek_write), fragmentation from parallel segment writes, free-space checks.
- Links and TOCTOU: symlinks, junctions, reparse points; openat2 RESOLVE_BENEATH, cap-std; locking (flock/fcntl/LockFileEx; Windows sharing modes and antivirus interference); watching (inotify limits, FSEvents, ReadDirectoryChangesW overflow; notify crate caveats).
- Trash/recycle (freedesktop Trash spec, IFileOperation, macOS), extended attributes and provenance metadata (Zone.Identifier format, com.apple.quarantine, kMDItemWhereFroms, user.xdg.origin.url / referrer.url from the freedesktop common-xattrs spec), xattr support by filesystem, preservation through copy/zip.
- File identification (WHATWG MIME Sniffing, libmagic, infer, Google Magika paper and Rust crate; extension/content mismatch policy). Search/indexing (Windows Search, Spotlight, Everything/USN, tantivy).
- Rules engines (Hazel, File Juggler, organize, Maid) and a design for Zeus's rule DSL: match -> actions, dry-run plan diff, idempotence, undo journal and move-journal format. Duplicate finders (fclones, czkawka, rmlint, jdupes) algorithms.
- Network filesystems and cloud-synced folders (OneDrive/Dropbox placeholders, Files On-Demand reparse points, never write .part into synced folders; SMB/NFS rename and fsync caveats), removable media, quotas; OS protections that block writes (Windows Controlled Folder Access, macOS TCC Downloads prompts, Flatpak/Snap confinement).
- Deliverable: a 'Zeus filesystem contract' checklist and a cross-platform test matrix.` },

{ slug: 'conversion-parsing-and-sandboxed-pipelines', file: '17-conversion-parsing-and-sandboxed-pipelines.md', title: 'Conversion, parsing and sandboxed pipelines',
scope: `
- Tool landscape with security/robustness notes: FFmpeg (ffprobe JSON, -c copy, hwaccel, protocol whitelist and HLS SSRF/LFI class of issues, CVE history), Pandoc (--sandbox, Lua filters), LibreOffice headless (profile dir, concurrency limits, macro security, unoserver), ImageMagick (policy.xml, ImageTragick) vs libvips vs Rust image, Ghostscript, Poppler/MuPDF/PDFium, qpdf, OCR engines (Tesseract, OCRmyPDF, PaddleOCR, docTR).
- Modern document parsers for LLM ingestion (verify IDs/benchmarks): Docling, Marker, MinerU, Unstructured, MarkItDown, Nougat, olmOCR, Mistral OCR, LlamaParse; comparative benchmark evidence (e.g. OmniDocBench; verify).
- Archive handling: libarchive and Rust zip/tar/sevenz crates, unrar licensing, zip64, zip bombs (Fifield WOOT 2019; limits on ratio, nesting, entry count, total size), tar path traversal and symlink attacks (CVE-2007-4559, recent Rust tar-rs issues; verify), encrypted archives, multi-volume archives.
- Detection (Tika, libmagic, Magika); conversion matrix and lossy/lossless taxonomy; deterministic output and provenance records (tool, version, args, hashes); idempotent re-conversion and content-hash caching.
- Sandboxing per OS: Linux (bubblewrap, Landlock, seccomp-bpf, namespaces, cgroups), macOS (sandbox-exec profiles, App Sandbox/XPC), Windows (AppContainer, Job Objects, restricted tokens, process mitigation policies), Chrome's sandbox as reference, WASM/WASI via wasmtime (ffmpeg.wasm / pandoc wasm feasibility and measured slowdown; verify); resource limits (CPU, memory, output size, wall-clock, no network), temp-dir hygiene, output re-validation.
- Licensing implications (FFmpeg LGPL/GPL builds, Pandoc GPL, LibreOffice MPL): subprocess vs linking. Discovery of installed tools vs bundling.
- Deliverables: a 'converter manifest' schema proposal (inputs/outputs MIME, tool requirements, sandbox profile, limits) and a per-tool hardening-flags table.` },

{ slug: 'provenance-dataset-and-content-governance-standards', file: '18-provenance-dataset-and-content-governance-standards.md', title: 'Provenance, dataset documentation and content-governance standards',
scope: `
- Provenance and packaging standards: W3C PROV-O/PROV-DM, RO-Crate, BagIt (RFC 8493), OCFL, PREMIS, WARC (ISO 28500) and WACZ; Croissant (MLCommons; current version, RAI extension, FileObject/FileSet/RecordSet and sha256 fields); Datasheets for Datasets, Data Statements, Dataset Nutrition Label; Data Provenance Initiative and 'Consent in Crisis' (verify arXiv IDs) and their license taxonomy; SPDX 3.0 AI/Dataset profiles, CycloneDX ML-BOM; C2PA / Content Credentials (spec version, manifests, bindings, c2pa-rs, trust lists), IPTC digital source type.
- Release/supply-chain attestations vs dataset attestations: Sigstore, in-toto, SLSA.
- Rights and opt-out signals: robots.txt RFC 9309, X-Robots-Tag, TDMRep (W3C CG), IETF AI Preferences (aipref) vocabulary and Content-Usage header (status), Really Simple Licensing (RSL 1.0), Cloudflare Content Signals Policy, ai.txt, Spawning Do-Not-Train, llms.txt, C2PA training-mining assertion, EU DSM Art. 4(3) machine-readable opt-out and AI Act GPAI Code of Practice commitments (verify).
- License detection (SPDX identifiers, scancode, rel=license, schema.org license, Creative Commons REL).
- Zeus's per-file provenance record schema (JSONL) with a crosswalk table to PROV / Croissant / RO-Crate; a manifest v0 JSON Schema proposal.
- Tamper-evident logging: hash chains, Merkle transparency logs (RFC 9162, Trillian, Rekor), RFC 3161 timestamps, OpenTimestamps; realistic threat model and limits; signing manifests (minisign, Sigstore, ssh-keygen -Y, age); replayability of runs from manifests; privacy (GDPR minimisation, stripping query secrets, pseudonymising URLs); chain-of-custody practice for journalists/legal use.` },

{ slug: 'legal-ethical-and-policy-landscape', file: '19-legal-ethical-and-policy-landscape.md', title: 'Legal, ethical and policy landscape (not legal advice)',
scope: `
Cite primary sources (opinions, statutes, official gazettes) and VERIFY status as of 2026-10; flag every uncertain item "verify with counsel".
- US: CFAA (Van Buren v. United States 2021; hiQ v. LinkedIn 9th Cir. and later history; Facebook v. Power Ventures; Meta v. Bright Data 2024; X Corp v. Bright Data 2024; Sandvig v. Barr), contract/ToS and browsewrap vs clickwrap, DMCA 1201 anti-circumvention and the 2024 triennial exemptions; recent bot/agent cases: Google v. SerpApi (Dec 2025), Reddit v. Anthropic (2025), Reddit v. SerpApi/Perplexity (Oct 2025), Amazon v. Perplexity (Nov 2025; Comet agent; find outcomes through 2026), NYT v. Microsoft/OpenAI, Thomson Reuters v. Ross, Bartz v. Anthropic (fair-use ruling and 2025 settlement over pirated downloads), Kadrey v. Meta; youtube-dl DMCA takedown 2020 and reinstatement.
- EU/UK/other: DSM Directive 2019/790 Arts. 3-4, Database Directive and Ryanair v PR Aviation, GDPR for scraped personal data and EDPB Opinion 28/2024, EU AI Act Art. 53 and the GPAI Code of Practice, UK CDPA s.29A and Getty v. Stability, GEMA v. OpenAI (Munich 2025), Japan Art. 30-4, Singapore, India DPDP, Bangladesh (Cyber Security Act 2023, personal data protection instruments, copyright; verify current status carefully), Brazil, Canada.
- Platform/app-store policy: Chrome Web Store (user data, limited use, single purpose), Mozilla, Microsoft Store, Apple notarization.
- Secondary liability for a tool with community extractors/workflows (Sony/Grokster doctrine; substantial non-infringing uses), DMCA agent and takedown handling for a community template registry, acceptable-use policy, responsible disclosure; JDownloader/yt-dlp/Gopeed precedent (including German decisions; verify).
- Export-control note for open-source crypto software; licensing: Apache-2.0 vs MPL-2.0 vs AGPL, GPL dependency boundaries (FFmpeg, Pandoc, unrar), patents, DCO vs CLA; trademark clearance method and an actual quick search for conflicts on 'Haul' (the former working name) and 'Zeus' in software/Class 9/42 (USPTO/EUIPO/WIPO; report what you find and how reliable).
- Ethics: robots norms, Menlo Report, ACM Code; recommended product guardrails and first-run notice outline; a risk heat map by feature (session handoff, logged-in crawling, community workflows, DRM boundary, LLM egress).` },

{ slug: 'rust-ecosystem-and-architecture-survey', file: '20-rust-ecosystem-and-architecture-survey.md', title: 'Rust ecosystem survey and architecture patterns',
scope: `
- A verified crate survey (current version, last release date, downloads, licence, maintenance health, verdict adopt/watch/avoid) across: async runtime and file IO (tokio, tokio::fs cost, compio/monoio/glommio, io_uring), HTTP (hyper 1.x, reqwest, h2, h3/quinn, hickory-resolver, rustls, rustls-platform-verifier, cookie_store, url/idna, tower), rate limiting (governor), retry (backon/backoff), storage (redb, rusqlite WAL, sled status, fjall, heed, turso), serialisation (serde, schemars, postcard, rkyv), hashing (sha2, blake3, crc32c, xxhash-rust, bao-tree/iroh-blobs), fastcdc, reed-solomon-simd, zstd, archives (zip, async_zip, tar, sevenz-rust2), MIME (infer, magika, mime_guess), filesystem (tempfile, fs4, notify, trash, camino, cap-std, xattr, directories, walkdir, ignore), IPC (interprocess, tokio named pipes, tarpc, jsonrpsee, tonic, axum), MCP (rmcp and alternatives), A2A SDKs, CLI/TUI (clap, ratatui, crossterm, indicatif, inquire, miette, tracing), GUI (Tauri 2 incl. sidecars/updater/single-instance; egui, iced, slint, dioxus), keyring, sandbox (landlock, seccompiler, extrasafe, wasmtime), scripting (rquickjs, boa, deno_core, mlua, rhai, extism, CEL), HTML parsing (scraper, html5ever, lol_html), html-to-markdown, browser control (chromiumoxide, headless_chrome, fantoccini), librqbit, cloud (object_store, aws-sdk-s3, opendal), LLM clients and embeddings (fastembed, ort, candle), tantivy, notifications/tray, autostart, platform APIs (windows, nix, objc2), release tooling (cargo-dist, cargo-packager, cargo-deny/audit/vet, cargo-semver-checks, nextest, llvm-cov, fuzz, mutants, release-plz), test tooling (proptest, loom, shuttle, turmoil, madsim, wiremock, insta, criterion/divan).
- Architecture patterns for a long-running download daemon: actors vs shared state, cancellation (CancellationToken), backpressure and bounded channels, structured concurrency, event-bus design and lagging subscribers, crash-only design, journal + replay, plugin boundaries (trait objects vs WASM vs subprocess), workspace/crate split guidance for a reusable engine-core, MSRV policy, feature flags, semver and API stability for a library meant for third-party embedding, binary-size and compile-time tuning with MEASURED numbers where you can build small probes (opt-level, LTO, strip, panic=abort, rustls vs native-tls).
- Reference implementations to study with specific lessons: aria2, Gopeed, AB Download Manager, xet-core, iroh, pget, hf_transfer, rqbit, uv, cargo, curl.` },

{ slug: 'competitor-and-reference-implementation-teardowns', file: '21-competitor-and-reference-implementation-teardowns.md', title: 'Competitor and reference-implementation teardowns',
scope: `
Source-level / doc-level teardown with live GitHub data (stars, last release, commit activity, open issue counts fetched today):
- aria2 (split, min-split-size, max-connection-per-server, lowest-speed-limit, piece-length, file-allocation, stream-piece-selector, uri-selector feedback/adaptive, RPC and notifications, .aria2 control-file, auto-file-renaming, checksum/Metalink), Gopeed (Go engine, fetcher interface, REST + MCP + JS extensions, manifest, hooks, cookie/header support; the MCP PR #1471 tools and current beta status), AB Download Manager (Kotlin; browser integration architecture, queues/scheduler), Motrix/AriaNg/Persepolis, XDM, Free Download Manager, JDownloader 2 (plugin system, LinkGrabber, premium accounts, legal history), IDM (only from public sources: segmentation, browser integration extension + native host), yt-dlp (downloader modules, fragment concurrency), gallery-dl, DownThemAll!, pget (Replicate), hf_transfer / hf-xet / xet-core (CAS, chunking, adaptive concurrency controller: read the code and describe the actual algorithm), uv, cargo multiplexing, Chrome built-in parallel download, curl --parallel, wget2, lftp, axel, rclone multi-thread-streams, aws s3 / s3transfer, azcopy, gcloud storage sliced downloads, croc.
- Agent-side: Gopeed MCP, yt-dlp MCP servers, Playwright MCP (download tools/flags and CURRENT state of issues #154/#953/#1754/#1768), Chrome DevTools MCP, browser-use (downloads_path, FileSystem, available_file_paths), Stagehand, Firecrawl, Apify actors, Hugging Face Hub CLI, filesystem MCP; thin 'AI file manager' products (Hazel, Local File Organizer, AI renamers, Sparkle, File Pilot).
- For each: what to copy, what to avoid, what is a moat; an updated feature-gap matrix; maintainers' roadmap signals regarding session handoff/MCP; an 'upstream engagement plan'. Cite repo files, issues and releases by URL.` },

{ slug: 'desktop-os-integration-packaging-distribution', file: '22-desktop-os-integration-packaging-and-distribution.md', title: 'Desktop OS integration, packaging, signing and distribution',
scope: `
- Daemon lifecycle per OS: Windows (per-user install without admin; Run key vs Task Scheduler vs service; named-pipe ACLs; toast notifications and AUMID; tray; installers MSI/MSIX/NSIS/WiX/Inno; winget/Scoop/Chocolatey), macOS (LaunchAgent, SMAppService login items, hardened runtime, entitlements, Developer ID, notarytool, universal binaries, Gatekeeper/quarantine, Homebrew formula/cask, TCC folder prompts), Linux (systemd --user, XDG autostart, D-Bus activation, AppImage, Flatpak with portals, Snap, deb/rpm/AUR/Nix).
- Code signing and reputation: Authenticode OV vs EV and SmartScreen reputation as of 2026 (verify), Azure Trusted Signing / Artifact Signing eligibility (verify), SignPath Foundation OSS programme, Sigstore for binaries; Microsoft Defender false-positive submission; AMSI; why a cookie-handling tool risks infostealer heuristics and the mitigations.
- Native messaging host registration: a table of per-browser x per-OS manifest locations and Windows registry keys (Chrome, Edge, Brave, Vivaldi, Opera, Chromium, Firefox; user vs system scope), Chrome allowed_origins vs Firefox allowed_extensions, Flatpak/Snap browser caveats (including the Snap Firefox problem and any xdg-desktop-portal native-messaging work; verify), spawning/attaching to the daemon from the native host (single-instance lock, pidfile, race handling).
- Updates: Tauri updater, self_update, cargo-dist installers, package managers, signature verification; extension distribution via Chrome Web Store (account, review, privacy disclosures, enterprise force-install), Firefox AMO (listed vs unlisted signing), Edge Add-ons.
- Tauri 2 specifics: WebView2 on Windows, WKWebView, WebKitGTK 4.1 and the distro matrix (Wayland/NVIDIA issues), bundle sizes, sidecars, capabilities/permissions, CSP, IPC.
- Power management (sleep inhibition APIs), metered networks, battery; accessibility; i18n including Bangla rendering; opt-in crash reporting; telemetry default-off.
- Release engineering: cargo-dist, GitHub Actions matrix, reproducible builds, SBOM (CycloneDX), SLSA provenance, gh attestation, checksums/minisign, CI cost of Windows/macOS runners. Provide a per-OS 'install and first-run' flow.` },

{ slug: 'testing-verification-and-benchmarking-methodology', file: '23-testing-verification-and-benchmarking-methodology.md', title: 'Testing, formal verification and benchmarking methodology',
scope: `
- Deterministic simulation testing (FoundationDB-style; turmoil, madsim, shuttle, loom), fault injection (toxiproxy, Linux tc netem/iptables, Windows clumsy/WinDivert), an in-process mock-server behaviour catalogue as a TABLE: range lies, ETag change mid-download, 200-instead-of-206, truncated bodies, resets, per-connection throttles, Retry-After, expiring signed URLs, redirects, chunked + gzip with ranges, HEAD disagreements, Content-Length mismatch.
- Property-based testing of segment-planner invariants (coverage, no overlap, monotonic commit); model checking the journal/commit protocol (TLA+/Quint/stateright; outline an actual spec of write -> fsync -> commit-range -> rename with crash points); crash-injection (failpoints, dm-flakey/LazyFS); fuzzing targets (Content-Range/Content-Disposition/Link parsers, URL normalisation, native-messaging frame decoder, MCP inputs, archive extractors); differential testing vs curl/aria2/browser; conformance suites (MCP conformance, A2A TCK; verify).
- Extension E2E testing (Playwright with extensions, Chrome for Testing, web-ext, MV3 service-worker testing patterns); native-messaging integration tests.
- Benchmark methodology: warmup, repetitions, confidence intervals, hyperfine, criterion/divan; network rigs; real-world targets (public mirrors, kernel.org, Hugging Face public files, GitHub release assets, public S3 buckets); metrics (throughput vs N, CPU, idle/loaded RSS, binary size, TTFB, resume correctness after kill -9 over N trials, last-1% tail time, write amplification); baselines (curl, aria2c -x16, wget2, IDM, AB DM, Gopeed, browsers) and fairness of comparison; Windows Defender real-time scanning effects.
- 'DownloadBench' failure-corpus design; agent-level evals (MCP tool-use evals with success + token cost metrics, AgentDojo-style prompt-injection regression suite for download scenarios, golden transcripts); coverage and mutation testing; CI strategy (OS matrix, flaky network tests quarantined, nightly soak, Miri, sanitizers); release gates and a security-invariant review checklist (link to the security doc).
- Provide concrete test plans.` },

{ slug: 'business-model-licensing-and-open-core-strategy', file: '24-business-model-licensing-and-open-core-strategy.md', title: 'Open-core business model, licensing and go-to-market evidence',
scope: `
- Evidence on what monetises developer tools in 2025-2026 with numbers where public: Supabase, Sentry (FSL), PostHog, GitLab, Grafana, Tailscale (OSS client, closed coordination server, Headscale), Obsidian (sync/publish), Raycast, Warp, Zed, Bun, Astral (uv/ruff), Deno, Charm; download-tool precedents (IDM pricing, Gopeed/AB DM/Motrix donations, yt-dlp sponsorship, JDownloader funding).
- License-change history and community reaction (HashiCorp BSL -> OpenTofu, Elastic SSPL -> AGPL, Redis RSAL/SSPL -> AGPL 2025, Sentry FSL, MongoDB SSPL); license options for Zeus (Apache-2.0 + trademark policy vs MPL-2.0 vs AGPL for server components vs FSL for paid components), DCO vs CLA and relicensing risk, patent grant.
- Monetisation consistent with research.md (Pro sync, Team policy/audit, Enterprise, OEM) with pricing evidence; funding sources (GitHub Sponsors, Open Collective, NLnet/NGI Zero, Sovereign Tech Agency, GitHub Secure Open Source Fund, Alpha-Omega, FLOSS/fund, AI-lab OSS programmes; verify each is open and eligible); dev-tool GTM benchmarks (stars-to-users, MCP registry listing effects, Show HN outcomes).
- Distribution channels: MCP registry, Smithery, PulseMCP, mcp.so, awesome lists, Claude Code plugin/skill marketplaces, Cursor directory, VS Code MCP gallery, Docker MCP catalog.
- Community governance: CONTRIBUTING, code of conduct, RFC process, SECURITY policy, bus-factor mitigation, trademark strategy.
- Design the 5 validation experiments from research.md in detail (instrumentation WITHOUT telemetry, survey, interview script for Playwright MCP users, landing-page tests, upstream conversations) and a risk register v2 with leading indicators and kill criteria. Verify the funding facts quoted in research.md. Be honest about uncertainty.` },

{ slug: 'terminal-orchestration-and-cli-tui-design', file: '25-terminal-orchestration-and-cli-tui-design.md', title: 'Terminal orchestration: CLI and TUI design for humans and agents',
scope: `
- CLI design for both humans and agents: clap; --json and NDJSON event streams; stable exit codes; machine-readable errors; NO_COLOR/CLICOLOR; tty detection; stdin handling; idempotency; --dry-run/--yes; config precedence; XDG dirs; completions; man pages; stderr vs stdout progress rules. Evidence on why agents often prefer CLIs over MCP (token cost, reliability) with sources.
- TUI frameworks and architecture (ratatui, TEA/component patterns, async event loops, tui-realm), Windows terminal quirks (conhost vs Windows Terminal, VT enablement, Unicode width, complex scripts / Bangla rendering limits; verify), mouse/resize/alt-screen.
- TUI UX for download managers with reference designs (aria2 TUIs, trippy, bandwhich, btop, yazi, gitui, lazygit, k9s, posting, oxker): keybindings, 10 fps progress, sparklines, torrent-style segment bitmaps.
- Terminal protocols: OSC 8 hyperlinks, OSC 9;4 progress, OSC 52, kitty graphics / sixel, bracketed paste, synchronized output.
- TRUST SURFACE IN THE TERMINAL: secure approval prompts reading from /dev/tty or CONIN$ (not stdin) so a piping agent cannot auto-approve; compare with sudo/polkit behaviour; failure cases (no tty, remote, CI).
- Orchestration from the terminal: job control, workflow run, watch/--follow, attach/detach, daemon discovery, remote daemon over SSH (stdio tunnel), shell integration, curl/wget/aria2 compatibility flags, clipboard watchers; agent-panel patterns (streaming markdown rendering: termimad, tui-markdown; diff previews); accessibility; plan -> dry-run diff -> apply UX (terraform plan, git add -p, jj).
- Deliverables: a proposed CLI command tree and an NDJSON event schema.` },

{ slug: 'workflow-engine-dsl-and-durable-execution', file: '26-workflow-engine-dsl-and-durable-execution.md', title: 'Workflow engine, DSL and durable execution for scraping/download plans',
scope: `
- Survey of existing DSLs and runners with design lessons: Apify Actor input schema, Scrapy, Crawlee routers, Gopeed extension manifest, JDownloader LinkGrabber rules, Playwright codegen, Nuclei templates (declarative community-shared YAML with matchers: sharing/safety/review model), n8n/Huginn/Node-RED, GitHub Actions, Argo/Tekton, Temporal/Restate/Inngest/DBOS (durable execution), Dagster/Airflow/Prefect, Kestra, make/just (idempotent targets), WebScraper.io sitemaps.
- Formal design: typed plan IR (sources -> pagination -> item extraction -> filters -> fetch -> post-process -> sink) with JSON Schema; selector languages (CSS, XPath, JSONPath, JMESPath, jq, regex); expression sandboxing (CEL via cel-rust, Starlark, QuickJS, WASM) for safety and determinism; limits and budgets (pages/files/bytes/time/rate/depth).
- Checkpointing and resume (idempotent steps, journal, exactly-once vs at-least-once, idempotency keys, replay), crawl-frontier state schema, failure semantics (retry classes, skip vs abort, dead-letter queue), human gates (approve plan -> dry-run sample -> full run; canary), drift detection and self-repair loops (LLM proposes selector patch -> validated against stored samples -> human approval), run reports (coverage, yield, errors, cost).
- Community template sharing: registry, signing, review, permission manifest (domains, max rate, session needed).
- LLM planning loop: page-sample compression (DOM simplification, accessibility tree, HTML pruning, token budgets; AutoWebGLM / Agent-E style distillation; verify), the host-agent-fills-selectors pattern vs MCP sampling vs built-in LLM; prompt/schema design; plan-quality evaluation; run cost/time estimation (link to the scheduling-math doc).
- Provide a JSON Schema draft for plan v0 and THREE worked example workflows with expected dry-run output: a paginated PDF portal behind login; GitHub release assets; an arXiv bulk download that respects arXiv's published bulk-access policy (verify what arXiv actually recommends: OAI-PMH, S3 requester-pays bulk data, API limits).` },

{ slug: 'llm-data-mining-pipelines-and-dataset-formats', file: '27-llm-data-mining-pipelines-and-dataset-formats.md', title: 'LLM data-mining pipelines, sanctioned bulk sources and dataset formats',
scope: `
- Where a download manager adds value in acquisition -> clean -> dedupe -> filter -> shard pipelines. Reference pipelines (verify IDs): FineWeb and datatrove, RefinedWeb, Dolma, The Pile, RedPajama-V2, DCLM, Common Pile, NeMo Curator, Ray Data, HF datasets, Common Crawl (WARC/WET/WAT, CDX index API).
- Sanctioned bulk-access routes per source (the product should steer users from scraping to official bulk endpoints: a 'prefer official bulk source' resolver feature): arXiv bulk data (S3 requester-pays, Kaggle, OAI-PMH), S2ORC/unarXive, PubMed Central OA, OpenAlex snapshots, CORE, Unpaywall, Wikipedia/Wikimedia dumps, Project Gutenberg, Internet Archive (ia CLI, rate limits), Zenodo/Figshare/OSF/Dataverse (resumable, DOIs, checksums), Kaggle API, Hugging Face datasets, Academic Torrents. For each: access method, size, auth, rate limits, licence.
- Dataset formats and shard-size math: Parquet/Arrow/Lance, JSONL(.zst), WebDataset tar shards, MosaicML MDS, TFRecord, HF cache, Croissant; manifests and lineage (link to the provenance doc).
- Corpus-scale quality methods: exact dedup, MinHash-LSH parameter (b, r) selection maths with threshold curve, suffix-array dedup, fastText quality/LID classifiers, PII scrubbing, toxicity filters; licence/rights filtering using provenance metadata; reproducible 'dataset recipes' (workflow + manifest + seeds).
- Personal-archive use cases with exact mechanics: Google Takeout (multi-part zips, link expiry, sign-in re-auth), Facebook/Instagram/X/Reddit data exports, GDPR Art. 20 exports; why they fit session-aware resumable downloading; failure modes observed.
- MCP resources/prompts for dataset workflows; agent-driven dataset-building papers 2025-2026 (search and verify); cost estimates for LLM-assisted classification of N files (token math; local small model throughput) with explicit assumptions.
- Provide THREE reference dataset recipes expressed as Zeus workflows.` },

{ slug: 'trust-surface-approvals-and-gui-architecture', file: '28-trust-surface-approvals-and-gui-architecture.md', title: 'Trust surface, human approvals and GUI architecture',
scope: `
- The problem: approvals (session grants, deletes, workflow runs) must be confirmable where an agent or injected prompt cannot spoof the content. Survey consent/approval UX research (consent and warning fatigue, Android permission research, UAC secure desktop, sudo/polkit, macOS TCC, WebAuthn user presence, Windows Hello), 'confused deputy' and 'approval text spoofing' in agent UIs, MCP elicitation limits, OS notification action buttons and their spoofability.
- Candidate trust surfaces for Zeus: extension popup (what the page/other extensions can and cannot spoof), terminal prompt on /dev/tty or CONIN$, OS-native toast with daemon-computed facts, Tauri GUI window, and a hardware-key/OS-auth confirmation (WebAuthn / Windows Hello / Touch ID) option; compare threat models in a table; specify the 'daemon-computed facts only' principle (origin, file count, bytes, destination, expiry) with example screens.
- GUI architecture: thin-client rule (daemon API is the single contract; no GUI-only features except approvals); Tauri 2 security model (capabilities, CSP, isolation pattern, IPC allowlists), serving the UI from the daemon on localhost (token, Origin checks, DNS-rebinding defences), WebView2/WKWebView/WebKitGTK constraints; egui/iced/slint/dioxus alternatives and when to prefer them.
- MCP Apps (ui:// resources, sandboxed iframes, CSP, postMessage bridge, host support matrix as of 2026-10; verify) as a low-cost embedded GUI: what to render (job progress, plan preview) and what must never be approvable there.
- Agent-activity UX: inbox, per-agent permissions and budgets, hash-chained audit view, kill switch; plan preview and dry-run diff visualisation; A2A peer/trust views; accessibility and i18n (including Bangla).
- Deliverables: a trust-surface threat table, example approval screens in ASCII/markdown mock form, and the human-only approval protocol (what the daemon signs, how approvals are bound to a request hash and expire).` },
]

const researchPrompt = (d) => `${COMMON}

YOUR DOCUMENT
- Output path: ${ROOT}\\${d.file}
- Title: ${d.title}
- Slug: ${d.slug}

SCOPE (cover every item; add what a world-class engineer or researcher on this topic would add)
${d.scope}

Remember: verify before you cite; run refcheck.mjs and mdcheck.mjs on your file before finishing; end with the structured summary.`

const verifyPrompt = (d, s) => `You are an adversarial fact-checker and editor. ASSUME the author (an AI) made mistakes: wrong arXiv IDs, wrong RFC numbers or obsoleted RFCs, invented version numbers or dates, wrong maths, unsupported overclaims, links that do not say what is claimed. Find them and FIX THEM IN PLACE.

File: ${ROOT}\\${d.file}
Topic: ${d.title}
Today is 2026-10-05. WebSearch and WebFetch are deferred tools: first call ToolSearch with query "select:WebSearch,WebFetch". Helper scripts: node "${SP}\\refcheck.mjs" <file> and node "${SP}\\mdcheck.mjs" <file>. GitHub REST API and crates.io API work unauthenticated through PowerShell Invoke-WebRequest / curl (send a User-Agent header).

Procedure:
1. Read the whole file.
2. Run refcheck.mjs. For every arXiv ID / RFC compare the official title, authors and year with how the doc describes it. Fix mismatches: correct the ID if you can find the intended paper by title (arXiv search), otherwise remove it or tag [unverified]. Replace obsoleted RFCs with the current ones and adjust text where semantics changed.
3. Run mdcheck.mjs; fix structural issues (tables, sections, fences, emoji).
4. Recompute EVERY numerical / worked example with node -e. Fix wrong arithmetic and any prose built on it.
5. Select the ${LITE ? '10-15' : '15-25'} highest-impact factual claims that are NOT arXiv/RFC (version numbers, dates, CVE ids, crate versions, size limits, browser behaviours, legal case names and citations, statistics, star counts) and verify each against a primary source. Fix, soften (confidence tag), or remove what you cannot verify. Check that URLs cited for specific claims resolve and actually support the claim.
6. Check each recommendation under "Implications for Zeus" is supported by cited evidence or explicitly labelled inference; fix overreach.
7. Confirm every scope item below is covered; if something required is missing or thin, ADD it yourself with sources.
Edit in place (Edit tool or rewriting sections), keep the voice, keep it dense. Set front matter status: verified (or verified-with-caveats) and add last_verified: 2026-10-05. Do not rename the file. Do not run git.

Scope that must be covered:
${d.scope}

The author's own summary flagged these as unverified or gaps: ${JSON.stringify((s && (s.unverified || [])).concat(s && s.gaps || []))}

Return the structured verification report. Use verdict needs-rewrite only if the document is so wrong or thin that in-place editing cannot rescue it.`

const repairPrompt = (d, v) => `The document ${ROOT}\\${d.file} (topic: ${d.title}) failed adversarial verification with verdict needs-rewrite. Verifier report: ${JSON.stringify(v)}.
Load WebSearch/WebFetch via ToolSearch (query "select:WebSearch,WebFetch"). Rewrite / repair the document in place so that it meets the standard: every citation verified (node "${SP}\\refcheck.mjs"), structure linted (node "${SP}\\mdcheck.mjs"), all maths recomputed with node, all scope items covered, claims sourced or labelled inference. Scope:\n${d.scope}\nReturn the structured verification report for the repaired file (verdict pass / pass-with-fixes).`

// ---------- Audit of the original research.md (two independent auditors) ----------
const AUDIT_COMMON = `You are an adversarial auditor of ${APP}\\research.md (a market/engineering research document dated October 2026, written by an AI from web sources). Today is 2026-10-05. WebSearch and WebFetch are deferred tools: call ToolSearch with "select:WebSearch,WebFetch" first. GitHub REST API and crates.io API work through PowerShell Invoke-WebRequest / curl (User-Agent header required). Compute every number yourself with node -e. Primary sources over secondary. Output file (Write tool, absolute path): `
const AUDITS = [
{ slug: 'claims-audit-technical', file: '00a-claims-audit-technical.md', prompt: AUDIT_COMMON + `${ROOT}\\00a-claims-audit-technical.md
Audit the TECHNICAL claims: Chrome 127 app-bound encryption; DBSC and the "Chrome 146 public availability for Windows, April 2026" claim; MCP spec versions including "stable MCP 2026-07-28" and rmcp 3.x; A2A v1.0 / Linux Foundation / a2a-rs; Gopeed MCP (PR #1471, 2.0 beta, tool count); hf-xet and Xet chunking numbers (64KB, 312,500 chunks, adaptive 1-64 streams); Playwright MCP issue numbers/titles/dates/state (#154, #953, #1754, #1768) and browser-use issues (#3879, #3721, #3136, #1853), yt-dlp / gallery-dl / VidBee issue numbers; HTTP/Range/If-Range technical statements in section 4.1; throttling/HTTP2 statements; ALL mathematics in section 8 (8.1-8.14): recompute each example and report exact values, flagging the dedup collision figure in 8.14 and the Reed-Solomon figure in 8.13, the Mathis example, the hedging bound, the Little's law and EWMA examples; the crate claims in 4.4 (versions, status, sled claim); Chrome/Firefox cookie handling claims in 4.2; MV3 claims.
Format: front matter (title, slug, status: verified, last_verified: 2026-10-05); a TL;DR; a table | # | claim (with research.md section) | verdict (Confirmed / Partly / Wrong / Unverifiable) | evidence (source URL) | suggested correction |; then sections 'Maths recomputed' and 'Suggested patch list for research.md' (exact replacement text). Be precise and complete; audit every checkable claim in the sections listed, not a sample.
Return the structured summary.` },
{ slug: 'claims-audit-market', file: '00b-claims-audit-market-and-legal.md', prompt: AUDIT_COMMON + `${ROOT}\\00b-claims-audit-market-and-legal.md
Audit the MARKET, COMPETITOR, FUNDING and LEGAL claims: every product/stat in section 2.1 (IDM prices, AB DM stars and Chrome users, Gopeed stars and licence, Playwright MCP stars, etc., fetched live from GitHub/Chrome Web Store where possible); the Atlas launch date and the claimed 2026-08-09 shutdown; Comet; Browser Use seed ($17M, date, investors); Browserbase Series B ($40M, $300M, date, revenue claim); Firecrawl Series A and the $75M Series B (Sept 22, 2026) and user counts; market sizing figures in 2.4 (Mordor, Research and Markets, Business Research Insights, F5 Labs, PromptCloud) including whether quoted numbers match the sources; MCP adoption statistics (97M monthly SDK downloads, 10,000 servers, registry record counts, donation to the Agentic AI Foundation date) and the rmcp claim; the 'Why now' table; the TAM/SAM/SOM arithmetic (recompute); legal section 5.7 (Van Buren, hiQ v. LinkedIn history, Meta v. Bright Data, DMCA 1201, EU items, Bangladesh status) against primary sources, plus a short list of important legal developments of 2025-2026 that the section omits (Amazon v. Perplexity, Google v. SerpApi, Reddit cases, Bartz v. Anthropic, etc. with current status); source-list integrity: sample at least 20 of the 62 sources to check they exist and support what they are cited for.
Format: front matter (title, slug, status: verified, last_verified: 2026-10-05); a TL;DR; a table | # | claim (with research.md section) | verdict (Confirmed / Partly / Wrong / Unverifiable) | evidence (source URL) | suggested correction |; then 'Suggested patch list for research.md' (exact replacement text) and 'Material omissions'. Be precise and complete.
Return the structured summary.` },
]

const AUDIT_SUMMARY = {
  type: 'object',
  properties: {
    file: { type: 'string' },
    claimsChecked: { type: 'number' },
    wrong: { type: 'array', items: { type: 'string' } },
    partlyWrong: { type: 'array', items: { type: 'string' } },
    unverifiable: { type: 'array', items: { type: 'string' } },
    keyOmissions: { type: 'array', items: { type: 'string' } },
  },
  required: ['file', 'claimsChecked', 'wrong'],
}

// ---------- Stage runner shared by the main run and the gap-fill run ----------
async function researchThenVerify(docs, phaseR, phaseV) {
  return await pipeline(
    docs,
    async (d) => {
      if (VERIFY_ONLY.has(d.slug)) return { file: ROOT + '\\' + d.file, words: 0, keyFindings: [], gaps: [], unverified: [] }
      let s = await agent(researchPrompt(d), { label: 'research:' + d.slug, phase: phaseR, schema: SUMMARY })
      if (!s) { log('retrying research for ' + d.slug); s = await agent(researchPrompt(d), { label: 'research-retry:' + d.slug, phase: phaseR, schema: SUMMARY }) }
      return s
    },
    async (s, d) => {
      if (!s) return null
      if (args && args.noVerify) return { slug: d.slug, file: d.file, title: d.title, summary: s, verify: null }
      let v = await agent(verifyPrompt(d, s), { label: 'verify:' + d.slug, phase: phaseV, schema: VERIFY, effort: HIGH })
      if (v && v.verdict === 'needs-rewrite') {
        log('repairing ' + d.slug)
        const v2 = await agent(repairPrompt(d, v), { label: 'repair:' + d.slug, phase: phaseV, schema: VERIFY, effort: HIGH })
        if (v2) v = v2
      }
      return { slug: d.slug, file: d.file, title: d.title, summary: s, verify: v }
    },
  )
}

// ---------- Run: research + verify for all docs, audits in parallel ----------
phase('Research')
const TODO = DOCS.filter(d => !SKIP.has(d.slug) && (!ONLY || ONLY.has(d.slug))).sort((a, b) => prio(a) - prio(b))
const [results, audits] = await Promise.all([
  researchThenVerify(TODO, 'Research', 'Verify'),
  (args && args.skipAudits) ? Promise.resolve([]) : parallel(AUDITS.map(a => () => agent(a.prompt, { label: 'audit:' + a.slug, phase: 'Audit', schema: AUDIT_SUMMARY, effort: HIGH }))),
])
const done = results.filter(Boolean)
const missing = TODO.filter(d => !done.find(r => r.slug === d.slug)).map(d => d.slug)
log('docs completed: ' + done.length + '/' + TODO.length + (missing.length ? ' ; MISSING: ' + missing.join(', ') : ''))

// ---------- Critic ----------
phase('Critic')
const digest = done.map(r => `- ${r.file} (${r.title}): findings: ${(r.summary.keyFindings || []).slice(0, 4).join(' | ')} || gaps flagged: ${(r.summary.gaps || []).join(' | ')} || verifier: ${r.verify ? r.verify.verdict : 'n/a'}, ${r.verify ? r.verify.errorsFound.length : '?'} errors fixed`).join('\n')
const GAPS = {
  type: 'object',
  properties: {
    newDocs: {
      type: 'array',
      items: {
        type: 'object',
        properties: { slug: { type: 'string' }, file: { type: 'string' }, title: { type: 'string' }, scope: { type: 'string' }, rationale: { type: 'string' } },
        required: ['slug', 'file', 'title', 'scope', 'rationale'],
      },
    },
    contradictionsBetweenDocs: { type: 'array', items: { type: 'string' } },
    missingDocsFromFailures: { type: 'array', items: { type: 'string' } },
  },
  required: ['newDocs'],
}
const gaps = (args && args.noCritic) ? { newDocs: [] } : await agent(`You are the completeness critic for a research corpus in ${ROOT} supporting an open-source Rust project (Zeus; research.md calls it Zeus, its old working name): a session-aware download manager and file layer for AI agents.

The user's original request, condensed: research and document, as markdown files, ALL the download standards, mathematics, protocols, cutting-edge research, agent and AI protocols (MCP, A2A, etc.), papers and arXiv work relevant to: a lightweight Rust desktop download manager with persistent browser sessions, MCP connectivity, scraping workflow design (pagination, advanced scraping, tooling), file management, agent-to-agent protocols, file splitting, orchestrating file listing / downloading / converting via prompting, BYOK and subscription-based LLM access, terminal-based orchestration, LLM data-mining workflows, direct browser-to-download-manager handoff to convert or persistently retry failed downloads and to download from logged-in/walled sessions, a GUI plus agentic/A2A GUI, MCP Apps, human-only trust surfaces, and an open-core business model.

Existing docs (28 planned + 2 audits of the original research.md). Digest from the authors and verifiers:
${digest}
Docs that failed to produce: ${missing.join(', ') || 'none'}.

Task: list the docs that are STILL MISSING for this corpus to let an engineer build the product and a researcher trust it. Use Glob/Read on ${ROOT} to skim titles and headings (do not read every doc in full). Propose at most 8 NEW docs, numbered from 29 (file names like 29-<slug>.md), each with a precise scope list in the same style as the existing ones (rich bullet lists of required topics, seed leads to verify). Prefer genuinely new topics (for example: offline/low-bandwidth and censorship-resilient downloading; accessibility/i18n for Bangla and RTL; observability/telemetry-free diagnostics; update/rollback safety; cloud-drive APIs; email/attachment sources; media-library and metadata tagging; privacy-preserving local semantic search; whatever you find missing). Also list contradictions you notice between docs, and re-propose any doc listed as failed (use its original slug and file name). Return the structured result.`, { label: 'critic', phase: 'Critic', schema: GAPS, effort: HIGH })

// ---------- Gap fill ----------
phase('Gap-fill')
let gapResults = []
if (gaps && gaps.newDocs && gaps.newDocs.length) {
  const newDocs = gaps.newDocs.slice(0, 8)
  log('gap docs: ' + newDocs.map(d => d.slug).join(', '))
  gapResults = (await researchThenVerify(newDocs, 'Gap-fill', 'Gap-fill')).filter(Boolean)
}

// ---------- Index ----------
phase('Index')
const allDocs = done.concat(gapResults)
const index = (args && args.noIndex) ? null : await agent(`You are the corpus editor. All research docs are in ${ROOT} (files named NN-*.md plus 00a/00b audit docs). Today is 2026-10-05.
Create two files with the Write tool:
1. ${ROOT}\\README.md: the corpus index. Contents: purpose and scope (Zeus; formerly "Zeus" in research.md, link to ../../research.md as the original market/product research); how to read it (reading orders: 'I'm building the engine', 'I'm building the extension', 'I'm building agent integrations', 'I'm assessing risk/legal/business'); a table of every doc with a 2-3 sentence abstract (read each doc's TL;DR with Grep/Read - do not guess), status and confidence from front matter; a 'Top 40 design decisions and where they are justified' table (decision | doc | evidence strength) distilled from the 'Implications for Zeus' sections; a consolidated 'Corrections to research.md' list (from the docs and the 00a/00b audits); a glossary of the 40 most important terms; conventions (citation keys, confidence tags, front matter statuses); and 'How to re-verify' (the refcheck/mdcheck approach described in plain words; copy the two scripts from ${SP} into a new folder ${APP}\\tools\\research-checks\\ with a short README there).
2. ${ROOT}\\references.md: a consolidated bibliography GENERATED PARTLY BY SCRIPT: write a node script in ${SP} that scans all docs for arXiv IDs and RFC numbers, fetches official metadata (reuse logic from refcheck.mjs), and emits markdown tables: (a) RFCs: number | title | status | obsoleted-by | cited in docs; (b) arXiv papers: id | title | first author | year | cited in docs; flag any ID whose official title does not plausibly match the surrounding text (list those separately for manual fix and FIX them in the docs). Also add (c) a table of non-RFC standards and specs (W3C/WHATWG/BEP/ISO/etc.) by scanning the References sections, and (d) key repositories. Run it and paste/commit the generated output into references.md. Also copy the generator script to ${APP}\\tools\\research-checks\\build-references.mjs.
Finally run node "${SP}\\mdcheck.mjs" on every doc and report which still fail. Return a short structured summary.`, {
  label: 'index', phase: 'Index', effort: HIGH,
  schema: { type: 'object', properties: { files: { type: 'array', items: { type: 'string' } }, docsIndexed: { type: 'number' }, mdcheckFailures: { type: 'array', items: { type: 'string' } }, mismatchedCitationsFixed: { type: 'number' } }, required: ['files', 'docsIndexed'] },
})

return {
  completed: allDocs.map(r => ({ file: r.file, words: r.summary.words, refs: r.summary.referenceCount, verdict: r.verify && r.verify.verdict, errorsFixed: r.verify ? r.verify.errorsFound.length : null, unverifiedRemaining: r.verify ? r.verify.unverifiedRemaining : null })),
  missing,
  audits: audits.filter(Boolean),
  correctionsToResearchMd: done.flatMap(r => (r.summary.correctionsToResearchMd || []).map(c => r.slug + ': ' + c)),
  gaps,
  index,
}
