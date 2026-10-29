# Artifact lifecycle API

Implemented on 2026-10-21. This is an executable subset of spec 13: record tombstones, retention changes and verified copy materialization. The separate [maintenance profile](artifact-maintenance-api.md) adds reviewed bounded import/export, exact collection, repair and integrity checks plus retained audit replay. Hardlinks, reflinks and quarantine release remain outside this lifecycle API.

`artifacts.lifecycle.preview` takes an active visible `artifact_id` and one operation. It returns `{plan, plan_hash}` without changing the store, filesystem or approval queue. The plan names the exact artifact, BLAKE3, SHA-256, size, current quarantine and retention, the hash of the canonical API record export, the full requested effect, whether approval is needed, and an expiration 15 minutes in the future. `plan_hash` is the BLAKE3 of canonical JSON `{t:"zeus.artifact.lifecycle.v1",plan}`.

`artifacts.lifecycle.apply` takes that entire plan, its hash and a required `idempotency_key`. It recomputes the plan from current trusted state before executing or raising an approval. Changed records, altered approval requirements, moved roots, changed destinations, expired plans and key reuse with different parameters are refused. A service can operate only on artifacts it may already inspect; materialization also requires one of its registered write roots.

| Operation | Behavior | Human decision |
| --- | --- | --- |
| `{kind:"delete"}` | Tombstones the record with the core store's write-ahead audit. Keeps user files, materializations, lineage and shared CAS bytes. | Required. |
| `{kind:"set_retention",retention:"keep",retain_until:null}` | Protects bytes while this record stays active. | No, because protection is strengthened. |
| `{kind:"set_retention",retention:"evictable",retain_until:...}` | Changes protection for later collection. Removing `keep` or shortening/removing an existing deadline weakens protection. | Required when protection is weakened. |
| `{kind:"materialize",path:"/absolute/new-file"}` | Verifies content through the store and copies it to a new file inside an allowed root, with OS internet marks. No overwrites or hardlinks. | No. Quarantined artifacts are refused. |

The first apply of an effect needing approval returns `applied:false` and an `approval_id`. It executes nothing. The existing human terminal approval screen displays the complete artifact plan, previous protection and effect; a typed request code binds the decision. The facts use `destructive_op`, have no filesystem samples and explicitly set `to_trash:false`. The terminal distinguishes a record tombstone from deletion of a user file. Browser decision surfaces remain unavailable for these facts.

After a person approves, repeat the exact apply request with the same key. A pending retry reuses the existing approval. A completed retry returns the original receipt; it never recreates a file subsequently edited or removed by the user. The daemon persists a started checkpoint before the core effect. Interrupted record mutations reconcile only when the resulting record matches the exact intended mutation and all other record fields still match. An interrupted materialization whose destination exists returns `artifact.lifecycle_reconciliation_required`: inspect the destination and preview a new destination. It never adopts an unrelated file as a successful copy. A destination or partial-file collision always preserves the existing file.

The daemon's mutation gate serializes lifecycle decisions and their durable receipts. Large copies release that global gate, with a separate active-key guard preventing duplicate execution. Kill switch, credential and roots are checked again immediately before entering the core operation. Emergency stop and client revocation can proceed during a copy, but a copy already inside the core operation can finish and publish its verified file: the core API has no cancellation or pre-publication authorization callback. This interface does not claim atomic cancellation of an accepted materialization. The core store serializes its own transactions, verifies copy bytes, applies no-replace publication and recovers its materialization intents. Collection and physical reclamation remain outside this API.

CLI examples (use an actual artifact ID):

```sh
zeus --json artifacts materialize art_... /downloads/report-copy.pdf > copy-plan.json
zeus artifacts apply copy-plan.json --idempotency-key copy-report-1

zeus --json artifacts retention art_... keep > keep-plan.json
zeus artifacts apply keep-plan.json --idempotency-key keep-report-1

zeus --json artifacts delete art_... > delete-plan.json
zeus artifacts apply delete-plan.json --idempotency-key delete-report-1
zeus approvals approve ap_...
zeus artifacts apply delete-plan.json --idempotency-key delete-report-1
```

`artifacts apply` accepts only a regular JSON preview file of at most 64 KiB. Global `--dry-run` loads and displays the preview without calling the mutation method. The CLI never decides lifecycle approvals automatically and never uses `--yes` to answer them.

Behavioral tests cover strengthened versus weakened protection, exact approval facts and typed confirmation, forged approval flags, shared content and user-file preservation, confinement and collisions, later user edits on receipt replay, stale snapshots, expiration, key conflicts and an interrupted copy across a daemon restart. Native-platform filesystem acceptance remains part of the wider release matrix.
