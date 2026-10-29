# Zeus 0.1 reference analysis

Investigation date: 28 October 2026. This report separates static observations, documentation and independent Zeus design decisions. Native target programs were not executed, injected into or modified. Raw proprietary analysis remains in the private `D:/zeus-alpha/evidence` directory and is not included in this repository. No activation data, personal indexes, settings or browser profiles were read.

## Target identity and tools

| Target | Installed version | Copied bytes | SHA-256 |
| --- | --- | ---: | --- |
| IDM `IDMan.exe` (x86 PE) | 6.42.64.2 | 6,057,328 | `d1a955acacb65a6ea72ad3cbf06dc35ba48245d04da6659d93f9ea03d165f199` |
| Everything `Everything64.exe` (x64 PE) | 1.5.0.1391a | 5,713,928 | `bdacc22ca03f5cee95fd8d3abf2bda3b5206b6b5ec086234a01cb562b21e58df` |

Rea 6.8.0 was acquired from npm with its SHA-512 integrity verified. A portable Node 24.22.1 Windows runtime was verified against Node's official SHA-256 manifest. Both are local to D:, without global MCP registration or client-configuration changes. Rea's read-only Windows Ghidra provider explicitly admits Ghidra 12.1.x and is verified upstream against 12.1.4; current Ghidra 12.2 is not a compatible substitute. Portable Temurin JDK 25.0.5+8 and Ghidra 12.1.4 have been verified and unpacked. Focused read-only native function analysis has completed for both copied executables. Local `D:/zeus-alpha/rea.cmd` and `D:/zeus-alpha/ghidra.cmd` launchers select these runtimes without changing system PATH. A source fork checkout does not supply the runnable Ghidra release.

The owner's Ghidra fork is cloned at `D:/ghidra`, origin `ArefinAlter/ghidra`, commit `1b9e58a915cf867e106495eaa2a4b18eb4971c9d`. Rea's fork is cloned at `D:/rea`, origin `ArefinAlter/rea`, commit `13fba271e6e14bf6235379564eb9d32aee7049d5`; the CLI above is a separately pinned published artifact. Both clones have repository-local long-path support and upstream remotes, leaving global Git settings unchanged.

## Evidence ledger

| Question | Evidence | What is established | Remaining boundary |
| --- | --- | --- | --- |
| IDM artifact/API inventory | Rea `inspect_pe_imports`, `ev_1a5a9cef6afd5c3bb6b1046b013f98f36d82c4289d15d0697ff2819b87802e08`; execution-free export/resource inventories in the same private target folder | The selected executable is a native x86 PE and contains file/device/process and WinINet URL/cookie API import declarations. | Imports do not prove reachable execution, dynamic scheduling or performance. No cookie state was read. |
| IDM range/chunk/media seeds | Bounded printable ASCII/UTF-16LE scan of the exact hashed executable, private `idm-string-seeds.json` | Static literals cover HTTP ranges/validators, free-chunk and connection decisions, ordered chunk assembly/rollback and HLS playlist tags, including variants, media, initialization maps and byte ranges. | String presence alone does not establish implementation correctness, actual control flow, supported sites, encryption handling or a particular splitting algorithm. The focused Ghidra results below establish particular control-flow paths, not general runtime acceptance. |
| IDM media component boundaries | Rea export inventories: `idmvconv.dll`, `ev_5013a26b8d1ed73d6c3a2a5d35f09fcc9743c3f526cfb34edb2f09c821dbc5e4`; `IDMVMPrs.dll`, `ev_f6cd581011240d0d45d210528c6b553e01282b8ad2013ddd09fd367319f1ae10` | One component declares format checks, container conversion, separate-stream mux and progress exports; another declares an MPD-parser interface and XML functions. The separately inventoried `idmvs.dll` declares version/build getters. | Names and exports establish component declarations, not caller reachability or correctness. Do not infer video processing from a DLL filename. Focused native call paths remain pending. |
| Everything artifact/API inventory | Rea `inspect_pe_imports`, `ev_9a7bbe425590e50fec4132c51402adb98e2f362f0bf6445afc9b9aae37e5077e`; execution-free export/resource inventories | The selected executable is a native x64 PE with file/device, change-notification, named-pipe and window-message imports. | Imports do not prove journal use or complete index behavior. |
| Everything journal seeds | Bounded printable scan of the exact hashed executable, private `everything-string-seeds.json` | Embedded diagnostics refer to NTFS/ReFS USN reads, record length/name-offset/overflow validation, journal identity changes and USN query calls. | These are focused investigation seeds. They do not prove a complete recovery algorithm or its runtime behavior. The focused native results below establish selected journal/control paths; complete index recovery and accounting remain separate questions. |

All twelve Rea portable import/export/resource inventory commands (the two executables plus selected media component imports/exports) completed successfully. Complete Evidence records are retained, rather than relying on truncated CLI previews. File offsets in seed reports remain original-file offsets and must be resolved through Ghidra's observed mappings before selecting analysis addresses.

## Focused IDM function results

Ghidra 12.1.4 default analysis completed for the immutable copied executable. Rea observed 18,826 identified local/external procedures and 7,010 defined strings; these counts do not establish discovery of undefined code/data. Original PDB names/types were unavailable, so function addresses and data-flow observations are authoritative, while reconstructed field meanings remain inferences.

| Question | Static observation | Evidence |
| --- | --- | --- |
| Dynamic range allocation | A selector walks eligible chunk entries and recomputes a largest available gap. A separate connection-decision body calls it and checks whether sufficient capacity remains. The allocator calls that selector, constructs another chunk, derives a split boundary, optionally aligns to a caller-supplied granularity, adjusts the prior chunk's end and links/persists the new chunk. This is actual control flow, beyond diagnostic names. | Selector `0x558c10`: `ev_cba2eab077eb2c5a26b3f4d24615526048b35e138b3f85f4041bfd4ac1dde2ce`; connection decision `0x559f60`: `ev_69640db8e23acc27be4798a732dce0ee0fb756fb5e473d44fa45e27438017336`; allocator `0x55f500`: `ev_0260cc81d713015772f18ae4d6c69b321a518d0c5041d4ad1313474fd4ef87d1`. |
| HLS initialization and ranges | The selected parser branches on initialization-map and byte-range tags, parses a map URI and optional map range, and forwards range state into segment construction. Other branches track duration, media sequence, target duration, end-list and discontinuity state. | Parser `0x5a0600`: `ev_e6f9560b5b0d67a2a3fd00e15e64a87e1825a2312fc8030a32493a4d84db822f`; string inventory `ev_1d83153e255c9827e316848e31a98a3d5d7edceaebb0f4d3b6e1e3b33e55a7cb`. |
| Download versus container conversion | A separate executable function loads the selected converter DLL, resolves its progress-aware TS-to-MP4 entry, invokes it and unloads it, with missing-library/export error branches. Conversion is an actual separate caller path, rather than a guessed filename role. | Caller `0x5a7a40`: `ev_2838e3a2251fac7147c495c20f9b28a533f21113a0d03e476d23c87b7d4da6a6`, corroborated by the converter export inventory above. |

The first two scheduler diagnostics led to separate compiler-generated exception handlers. Those handlers were not treated as scheduler implementations; the investigation followed their enclosing bodies and actual callers before drawing the allocation conclusion. This avoids equating a familiar string with an algorithm.

The independent Zeus implication is to model remaining ranges explicitly, drain or fence replaced workers before reassignment, keep commit/recovery state authoritative, and preserve manifest-specified media boundaries. No proprietary thresholds, structure layouts or pseudocode were copied. A supplied alignment parameter does not prove universal codec/frame awareness. These observations do not prove IDM's runtime speed, site coverage, crash consistency or correct behavior for every manifest.

Complete IDM native evidence (17 retained records, 3,023,208 bytes) was exported to private `D:/zeus-alpha/evidence/idm/ghidra-evidence.json` before closing the target. Rea closed its ephemeral program successfully; the next target is a separate immutable Everything copy.

## Focused Everything function results

Ghidra default analysis completed for the exact copied x64 executable: 9,315 identified local/external procedures and 12,947 defined strings. The same defined-inventory and original-type/name limitations apply.

| Question | Static observation | Evidence |
| --- | --- | --- |
| USN queries | A real wrapper invokes `DeviceIoControl` with the public USN-query control code and checks success/error before returning. | `0x1401e66c0`, `ev_c60714d08e2e40a9972655114275d3926fb60d9ec8383f279e783391d89b10ed`. |
| Journal continuity | The monitor's failure path re-queries journal identity and compares it with saved identity. A mismatch takes a distinct invalidation/reset path. Journal deleted/not-active/entry-deleted errors are separately marked out of date; ordinary failures use delayed retry and handle cleanup. This does not prove every downstream rebuilding case. | `0x14022fbf0`, `ev_f1c432f2bd3e6efcc73b6d8ff69348d4f535cfdb6d7389a5a11ee4b7370e9377`. |
| Record parsing and identity | The selected NTFS update function checks returned-byte count, minimum record space/length and filename offset/length before walking records. It handles change-reason flags and file/parent references, with additional name/identity reconciliation helpers. These are observed checks, not a formal malformed-buffer correctness audit. | `0x1400a9af0`, `ev_542df0bc6dfa354d010ddcf46d68e9ce21570939c4a569ed738c4fca4f4dcc73`. |
| File identity and additional names | A selected helper opens a file by ID, reads handle-based logical size/timestamps/attributes, retrieves `FileNameInfo`, checks its name length and walks additional names with identity checks before recording them. | `0x1401ca370`, `ev_18b95b71765550dc6cc88bdcef29143341297fe5387eb8ab11c9af55a1c4cad7`. |
| Allocated versus logical totals | The bounded size-API and option-literal search does not establish the selected build's physical-allocation computation or hard-link-deduplicated totals. The observed handle metadata path explicitly reads logical file size. The existence of size-on-disk modes is not proof that an independent implementation produces the same accounting. | Search `ev_5964e51a4497e74aed2b0c1cc75983a1a3a6a489b40203455635d9ae90a2ab6e`; exact target-bound helper evidence above. |

Zeus must treat journal IDs/cursors as continuity evidence, reconcile after gaps or resets, bound every parsed record and maintain file identity separately from display paths. A read-only optional Everything query adapter does not itself implement a USN index or authorize filesystem mutations. File-size views must distinguish logical bytes, allocated bytes and unique physical accounting; none should be inferred from a filename index alone.

Complete Everything native evidence (18 retained records, 3,264,761 bytes) was exported to private `D:/zeus-alpha/evidence/everything/ghidra-evidence.json`. Both targets were closed successfully, no Ghidra runtime directories remain, and hashes of all five selected executable/DLL copies remained unchanged. The private Rea session was then stopped. Both fork source checkouts were verified clean with their expected origin/upstream remotes and repository-local long-path settings.

## Documented behavior and independent design

Everything documents a memory-resident name/path metadata index, optional indexed fields and persistent fast-sort indexes. Unindexed metadata is fetched on demand and may be slower. Its NTFS/ReFS index does not follow junctions; folder indexing supplies other filesystem/network-drive coverage. These are useful requirements for an independent scoped Zeus implementation, not evidence that Zeus already meets Everything's performance. [Everything indexes](https://www.voidtools.com/support/everything/indexes/).

Everything 1.5 documents a bounded index-change journal, including create, rename, move, modify and delete, with rename/move undo. Zeus should preserve operation receipts and verify identity/collisions before undo; observed filesystem changes alone do not authorize writes. [Everything Index Journal](https://www.voidtools.com/support/everything/index_journal/).

Everything's public SDK requires Everything to be running. SDK2 uses window-message IPC; SDK3 targets Everything 1.5 with named pipes and explicit default/named-instance connections. Its alpha uses the `1.5a` instance. Optional interoperability must respect installed-version capabilities and cannot replace Zeus's own scoped index/fallback. [Everything SDK](https://www.voidtools.com/support/everything/sdk/), [official SDK3 source](https://github.com/voidtools/everything_sdk3).

Both official SDK source archives and the release-matched ES 1.1.1.39 helper were acquired for the optional adapter; their source licenses are MIT and require retaining notices when redistributed. SDK2's synchronous query can wait indefinitely. SDK3 supports cross-thread client shutdown but has no public per-request timeout setter in the inspected header; the ES database-load timeout is not an overall operation deadline. The adapter needs its own bounded process lifecycle. No SDK/ES queries were run against the owner's personal index. [Official ES source](https://github.com/voidtools/ES), [ES release 1.1.1.39](https://github.com/voidtools/ES/releases/tag/1.1.1.39).

For Zeus, independent range scheduling must preserve strong validators, disjoint writes, durable commit boundaries and recovery. Media handling needs explicit playlist/representation selection, ordered assembly and accurate unsupported-content errors. No proprietary pseudocode is to be copied into the implementation, and the static reference findings do not replace loopback protocol and fault tests.

## Current limitations

This is a completed focused static investigation of the questions above, not a complete reverse engineering of every feature in either product. Runtime throughput, media-site coverage, malformed-input correctness, every journal rebuild/FRN graph case, physical-allocation and hard-link-dedup accounting remain unverified. These limits do not establish that a feature is absent. No target program was executed by the analysis tools, no user settings/index/history/profile was read, and proprietary pseudocode/layouts/constants are excluded from the Zeus implementation and this repository.
