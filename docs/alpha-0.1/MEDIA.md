# Zeus 0.1 alpha media acquisition

The native desktop's Link grabber includes a media review and a persistent acquisition list. An inspection reads only a URL the person supplies; it does not execute page scripts or inspect browser profiles. Signed source and segment URLs remain in native memory. The frontend and private checkpoints receive opaque identities, digests and redacted facts.

## Capability map

| Input or operation | Executable behavior | Limits |
|---|---|---|
| Direct media | Inspect, review, save and SHA-256 receipt | Anonymous HTTP(S). The normal Downloads queue is preferable for large single-file transfers and its adaptive range scheduler. |
| Page media discovery | Extract explicit media/manifest references from HTML markup, then inspect the chosen resource | No script execution or script-only player extraction. A discovered link does not start a download. |
| HLS master | Review declared bitrate/resolution and separate audio renditions | Up to 64 format choices. Nested masters require another explicit inspection. |
| Finite HLS | Ordered segment acquisition, initialization maps and exact byte ranges, including implicit consecutive ranges | Requires `EXT-X-ENDLIST`; up to 4,096 resources. Encryption, protected sessions, discontinuities, gaps and low-latency parts return explicit unsupported errors. |
| Static DASH | Select a representation; `SegmentList`, `$Number$`/`$Time$` templates, finite timelines and single-file representations | MP4/WebM tracks; one period. Dynamic/protected MPDs, negative/unbounded repeats and unsupported indexed addressing fail explicitly. Audio/video remain separate unless the person requests muxing. |
| Resume | Reinspect/select the same resource list, then explicitly choose a matching interrupted task | Exact expanded URL/range/order fingerprint must match. Retained files are rehashed; strong ETags are revalidated. Missing strong validators cause fresh segment downloads. Changed validators refuse mixed-version assembly. Changed signed URLs are a new plan; this is not an authentication-refresh claim. |
| Offline output recovery | Rebuild a complete set of retained segments, publish a durable converter-output receipt, or recognize identical output after a rename/checkpoint crash | Does not contact the source or run a helper. Every retained segment/converter output and any existing output are rehashed. Different existing files are preserved. |
| Stream-copy mux/repackage | Optional local FFmpeg over one completed media file or a video/audio pair; MP4 or MKV output | Explicit operator pin required. No arbitrary path, command, filter, codec or shell. Incompatible codecs fail. MP4 output is fragmented MP4. |
| Cancel and archive | Cancel in-flight acquisition/helper work; archive an inactive task's private staging cache | Archive preserves completed output. Startup never silently resumes network activity. |

Native HTTP requests check their exact origin before dispatch and every redirect. The first origin is the inspected URL's scheme/host/port; at most eight additional origins can be explicitly included. HTTPS-to-HTTP redirects are refused. Request headers and credentials cannot be supplied by the webview. These anonymous native routes do not implement browser-authenticated media capture; that requires the daemon's existing session authority and its own acceptance track.

Manifests are limited to two MiB, nesting/element counts are bounded, and expanded resource addresses are limited to sixteen MiB per catalogue. At most two media tasks and two inspections run together; a task uses one to eight resource requests. Individual segmented resources are limited to 512 MiB. A reviewed receive ceiling, up to 100 GiB, counts bytes across retries. Private segment receipts are committed only after file flush; assembly reads and hashes the same open files and uses an exclusive final rename in a configured download root. SHA-256 records internal byte integrity; it does not establish publisher authenticity or prove that a container will play in every player.

## Optional FFmpeg helper

The helper is disabled until an operator writes `app/media-helper.json` under this installation's app home. The native UI has a read-only helper-status route and cannot write a pin or trust an executable. A helper update requires an explicit operator pin change followed by a fresh mux review. Verify the build source and its published integrity data before configuring it. FFmpeg's own [download page](https://ffmpeg.org/download.html) links the available platform builds; Zeus does not bundle or silently install them.

The operator-owned configuration has this closed shape:

```json
{
  "version": 1,
  "enabled": true,
  "executable": "D:\\Tools\\ffmpeg\\ffmpeg.exe",
  "sha256": "the_64_hexadecimal_digit_sha256_verified_by_the_operator"
}
```

On Linux/macOS use an absolute executable path. Configuration is protected as a private application file; symlink/reparse configurations and helper executables are refused. Windows requires a local `.exe`; script wrappers and network paths are unavailable. The executable is rehashed before every launch and kept open through execution; Windows's share mode denies replacement while it runs.

Mux plans accept only completed local media task identities. Zeus copies and independently hashes the inputs into a private folder, limits their combined size to 100 GiB, and invokes fixed argument vectors with closed input demuxers, file-only input protocols, disabled MOV external data references and stream copying. Original files are preserved. The helper receives no output filename: its stdout passes through Zeus's hard byte ceiling, then its output is independently rehashed and published with a no-replace rename. A reviewed time limit covers staging, helper work and publication; cooperative checks cannot interrupt an operating-system call stalled inside a filesystem driver.

The profile tries ordinary stream copy first. AAC in ADTS/transport streams can require the fixed `aac_adtstoasc` framing filter. For MKV this fallback uses a private fragmented-MP4 intermediate so AAC configuration exists before the Matroska header is written. MP4 delays its header until that configuration is available, with two-second/16-MiB fragment cutoffs. At most three fixed helper invocations run; one cumulative helper-output ceiling counts failed attempts, intermediate bytes and final bytes, and one deadline covers the entire operation. This is framing/container conversion, not codec transcoding. A flushed converter-output SHA-256 receipt is committed before publication, allowing exact offline recovery after interruption.

The subprocess environment excludes inherited product credentials, API keys and user profile paths. Stdin is closed and diagnostic output is bounded and discarded. Cancellation/deadline closes a Windows Job Object or kills the private Unix process group. These controls and FFmpeg's protocol options are **not an OS filesystem or network sandbox**. This profile runs trusted, explicitly pinned native software on untrusted media. It does not complete the roadmap's general sandboxed-conversion requirement.

## Closed native routes

All paths below are under `/native/v1/media/`, served by the app's existing authenticated Tauri bridge (or the authenticated test harness). They do not add a daemon listener or approval authority.

| Method/path | Purpose |
|---|---|
| `POST inspect` | URL and optional explicit additional origins → opaque inspection and format facts. |
| `POST select` | Inspection/format identity → expanded private plan and safe review; page candidates may yield another selection. |
| `POST start` | Plan, configured root index, safe filename, byte ceiling, connection limit and idempotency key; optional explicit matching resume identity. |
| `GET tasks` | Redacted persistent progress, complete-output SHA-256 and checkpoint-corruption count. |
| `POST cancel` | Cooperative stop while retaining durable verified data. |
| `POST recover` | Reassemble/verify a fully retained acquisition or durable converter output without network/helper execution. |
| `POST archive` | Delete an inactive task's private cache and receipt, preserving its output. |
| `GET helper` | Read the operator-owned helper configuration's public identity and limits. |
| `POST mux-preview` | Review one complete task or a video/audio pair and MP4/MKV destination. |
| `POST mux-start` | Start that unchanged helper/source plan with a safe destination, output ceiling, deadline and idempotency key. |

Unexpected fields, invalid paths, expired plans and changed idempotent payloads are refused. The in-app/external MCP agent tool table remains daemon-backed: these native desktop routes are not advertised as authenticated agent media tools.

## Validation

On 28 October 2026 the final native regression run passed 26 tests: twelve unit/helper tests, four existing site-walk tests and ten media integration tests. The ordinary run leaves the helper-child fixture and explicit real-helper acceptance ignored; the child fixture is exercised by the supervision tests. Strict all-target Clippy with warnings denied and the frontend type check passed. Loopback checks cover HLS initialization/ranges, multiple DASH segments, validator changes, expired sources, origin limits, exact receive ceilings, destination-review changes, corruption preservation, cancellation, restart and offline publication recovery.

The separate opt-in real-helper acceptance passed using an isolated test-only FFmpeg 9.0.2 Windows executable. Its SHA-256 was `3256173f3f8bffd7df12227c68adf68025edb1832273a9530688a7bb1ed8edec`; the official-download-linked Gyan essentials ZIP matched its published SHA-256 `60f467265b1e312373dbcd92200c2618a74850f98d3d078e94296bb3fa2047ba`. Generated one-second H.264/AAC inputs were independently decoded before testing. The actual service acquired them from loopback, combined separate video/audio into MP4, repackaged MPEG-TS into MP4 and MKV, and verified two streams, AAC configuration/48-kHz rate, full decode and final SHA-256 receipts. The same run checked a hard cumulative output cap, changed-helper-pin refusal, deadline idempotency, archive preserving output and offline post-publication recovery with the helper disabled. Logs: `D:/zeus-alpha/logs/media-final-test.log`, `media-final-clippy.log` and `media-real-ffmpeg.log`.

Tests use loopback origins and isolated app homes. They do not touch the person's browser profile, existing media library or real helper configuration. This evidence establishes the tested finite anonymous formats and fixed stream-copy profile; it does not establish authenticated site extraction, arbitrary codecs, live recording, encrypted media or an OS sandbox.

Primary protocol references: [HLS RFC 8216](https://www.rfc-editor.org/rfc/rfc8216), [DASH-IF timing/addressing guidance](https://dashif.org/Guidelines-TimingModel/), [FFmpeg protocol options](https://ffmpeg.org/ffmpeg-protocols.html), and [FFmpeg format options](https://ffmpeg.org/ffmpeg-formats.html). The independent Ghidra/Rea findings are recorded in `REFERENCE-FINDINGS.md`; no proprietary implementation was copied.
