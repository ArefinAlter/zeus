# Artifact maintenance

This profile exposes bounded bundles, exact garbage collection and retained audit replay through the daemon, CLI and existing `zeus_artifacts` MCP tool. It reuses the content store's immutable identities, import evidence, retention, write-ahead intents and recovery. Store and record formats remain 1; higher versions are refused. There is no new format to migrate, and no speculative migration mutates an existing store.

## Saved plans and authority

`artifacts.maintenance.preview` is read-only. `artifacts.maintenance.apply` accepts the full plan/hash and an owner-scoped retry key. A plan expires after 15 minutes. The hash binds the operation, local store identity, retained root/parent/source file identities, and complete bundle or collection facts. Paths are confined to existing plain directories under currently authorized roots; links, traversal, state directories, overwrite and root grants are unavailable.

Operations are `export {artifact_ids,path,include_objects}`, `import {path}`, `collect {quota_pressure,limit}`, `repair {artifact_id,path}` and `scrub {artifact_ids}`. Export can include 1–64 visible active records. Services can export their own visible records; store-wide collection and bundle import require the local owner, whose credential may be an agent. A foreign producer's job IDs never establish local service ownership. Importing tombstones is refused so it cannot resurrect records.

Collection, repair and integrity checking create separate human `destructive_op` approval for their exact target list. Agents cannot decide it. The terminal shows permanent CAS byte removal, counts, exact digests/identities and prior reference revisions. Existing user files, materializations and provenance records remain. Changing protection, roots, object identity or the reviewed set invalidates the request. At most 128 objects may be selected, and approval plans must fit 12 KiB (select fewer objects when needed); `quota_pressure:false` observes the configured unreferenced grace period, while `true` can also select active evictable content. Keep/deadline/lineage protection and current intents are rechecked under the core writer lock. The write-ahead audit must succeed before deletion.

Repair verifies both content hashes from a retained source handle before staging up to 16 MiB. It restores only an active absent/damaged object without changing provenance, retention, quarantine or immutable identity. An interrupted repair revalidates the original record and source, then hashes already-present stored content before considering it complete.

Integrity checking accepts up to 64 visible active records, 16 MiB per observed object and 32 MiB total, subject to the same 12 KiB approval-plan cap. It binds each observed file identity and length (including truncated content), all current index/reference revisions and the store root. Every target is read and hashed through retained handles before effects. Mismatches move by retained no-replace rename into private damaged storage after a durable intent; user files and original metadata remain. Interrupted checks require a fresh preview of remaining present objects. Results identify verified and damaged digests.

## Bundle format

`.zeusab` version 1 is deliberately uncompressed:8-byte `ZEUSAB1\n` magic, a little-endian u32 canonical JSON manifest length, that manifest, and exact object bytes in manifest order. It is a data container, never an archive extracted into a directory. The only object member name is `objects/<lowercase BLAKE3>`; there is no link/directory/type, filesystem mode, executable command or compression field. Duplicate source identities, duplicate members, unknown manifest fields, path-like names, noncanonical JSON, trailing bytes, hash mismatches and future versions fail closed.

Budgets are 32 MiB total container/expanded data,4 MiB manifest,64 records and objects,256 KiB per record, and 16 MiB per object. Reads enforce the total cap even if a source grows. Every record and both hashes of every object are validated before import effects. Store conflicts and total additional capacity are checked before the first record; import never triggers automatic collection. This batch is not one cross-record transaction: a later I/O failure may leave earlier records committed, and the exact source-store/record binding recovers them on retry. Changed immutable evidence is a conflict. Bytes are neither overwritten nor used to repair an existing damaged object.

Metadata-only export/import works for records with absent bytes and preserves quarantine, provenance, source evidence, retention and deadlines. Imported metadata-only content is unverified unless this store already has verified bytes. Quarantined byte export is refused; equal local content and known inputs cannot shed quarantine through a foreign record. Imported IDs are local and import hops record the original source IDs. Exported provenance drops URL queries and session origins.

## Recovery, cancellation and event replay

Apply persists a started checkpoint before effects. Completed retries return their original receipt even if a user later edits or removes an exported file. A destination present after an interrupted export requires reconciliation; Zeus never adopts it as its output. Export publication uses exclusive creation, a verified retained source identity and no-replace rename. Interrupted import/collection returns `status:interrupted`, a stable error code and known committed record IDs or absent/remaining target counts. These observations do not prove which process removed a file. The next identical apply resumes only the originally reviewed targets and rechecks protection; it never selects newly eligible objects.

Emergency stop and credential revocation remain callable during accepted work. A check immediately before the blocking effect can return `cancelled_before_start`. Once the bounded core operation has begun it may finish; this API does not promise atomic cancellation or rollback of accepted multi-record work. An active-key guard survives caller disconnect and remains held through receipt persistence. An RPC lost after effects is recovered by the same saved plan and retry key. Collection whose approval/plan has expired needs a new review before remaining effects.

`artifacts.events` reads the oldest retained matching audit facts after `after_seq`, with 1–100 results, chain ID, next sequence, earliest retained sequence and a history-truncation indication. Events are scoped to visible records or the service's own actor. URLs, paths, credentials and arbitrary record metadata are omitted. `artifact.deleted`, `artifact.gc_sweep` and `artifact.scrub_intent` are explicitly `phase:intent`; they do not prove completion. Successful daemon lifecycle/maintenance paths append `*_completed` events bound to the plan hash. Other existing artifact actions are `phase:recorded`. A crash between event append and receipt persistence can repeat a completion event; consumers use its plan hash for deduplication. This is pull replay of retained audit history, not a complete event subscription or an independently signed attestation.

## Commands

```sh
zeus --json artifacts export /downloads/papers.zeusab art_... --include-objects > export-plan.json
zeus artifacts maintenance-apply export-plan.json --idempotency-key papers-export
zeus --json artifacts import /downloads/papers.zeusab > import-plan.json
zeus artifacts maintenance-apply import-plan.json --idempotency-key papers-import
zeus --json artifacts collect --limit 16 > collect-plan.json
zeus artifacts maintenance-apply collect-plan.json --idempotency-key collection-1
zeus approvals approve ap_...
zeus artifacts maintenance-apply collect-plan.json --idempotency-key collection-1
zeus --json artifacts repair art_... /downloads/verified-source.bin > repair-plan.json
zeus --json artifacts scrub art_... > scrub-plan.json
zeus artifacts events --after-seq 0 --limit 20
```

Global `--dry-run` on maintenance apply only displays the saved receipt; it does not request approval. MCP uses `action:maintenance`, phase `preview`, `apply` or `events`, and typed API fields under `request`. It has no arbitrary RPC or approval-decision path. The fifteen-tool catalogue remains within its wire budgets.

The previous generic core import/export, `collect_step`, `scrub_step`, and path-based repair remain available to embedders. New bundle and exact-target methods underpin these reviewed daemon paths. No newer store format currently exists to migrate to; an actual future migration still requires backups and a separately reviewed exact plan. This profile does not claim converters, models, classifiers or named collections.
