//! Restartable journal-to-audit, provenance and artifact consumers.
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use zeus_api::{ApiError, ApiResult};
use zeus_journal::Journal;
use zeus_types::*;
use zeus_verify::{
    audit::{AuditEntry, AuditLog, AuditOutcome, AuditSink},
    provenance::*,
};
fn fail() -> ApiError {
    ApiError::internal("durable provenance outbox unavailable")
}
fn error(e: impl ToEnvelope) -> ApiError {
    e.to_envelope().into()
}

pub(crate) struct Outbox {
    journal: Journal,
    audit: Arc<AuditLog>,
    artifacts: Arc<zeus_artifact::ArtifactStore>,
    roots: Vec<(PathBuf, ProvenanceWriter)>,
    /// Replay receipts recovered from the audit itself if fsync preceded the journal cursor commit.
    receipts: BTreeMap<u64, AuditHead>,
}
impl Outbox {
    pub fn open(
        _state: &Path,
        roots: &[PathBuf],
        journal: Journal,
        audit: Arc<AuditLog>,
        artifacts: Arc<zeus_artifact::ArtifactStore>,
    ) -> ApiResult<Self> {
        let meta = journal.meta().map_err(error)?;
        let head = audit.head().map_err(error)?.ok_or_else(fail)?;
        let floor = meta
            .audit_cursor
            .min(meta.manifest_cursor)
            .min(meta.artifact_cursor);
        let mut receipts = BTreeMap::new();
        // Normal restarts retain only receipts needed by unfinished consumers.
        let store_id = meta.store_id.to_string();
        audit
            .visit_records(|record| {
                if record.data.get("journal_store_id").and_then(|v| v.as_str()) == Some(&store_id)
                    && let Some(seq) = record
                        .data
                        .get("journal_seq")
                        .and_then(|v| v.as_u64())
                        .filter(|seq| *seq > floor)
                {
                    receipts.insert(
                        seq,
                        AuditHead {
                            chain_id: head.chain_id,
                            seq: record.seq,
                            hash: record.hash,
                        },
                    );
                }
                Ok(())
            })
            .map_err(error)?;
        let mut writers = Vec::new();
        for root in roots {
            let root = root.canonicalize().map_err(|_| fail())?;
            let header = ManifestHeader {
                record_type: HeaderTag::Header,
                manifest_version: 0,
                created_at: Timestamp::now(),
                generator: Generator {
                    name: "zeus".into(),
                    version: env!("CARGO_PKG_VERSION").into(),
                },
                run_id: None,
                root_label: None,
                privacy: Privacy {
                    url_query: UrlQueryPrivacy::Dropped,
                    session_origin_included: false,
                    os_metadata: OsMetadataMode::MarkOnly,
                },
                hash_algorithms: vec![HashAlgorithm::Sha256, HashAlgorithm::Blake3],
            };
            let writer = ProvenanceWriter::open(&root, header).map_err(error)?;
            writers.push((root, writer));
        }
        Ok(Self {
            journal,
            audit,
            artifacts,
            roots: writers,
            receipts,
        })
    }
    /// Process at most 256 durable events per call. Cursor advancement follows each durable side effect.
    pub fn tick(&mut self) -> ApiResult<()> {
        let meta = self.journal.meta().map_err(error)?;
        let start = meta
            .audit_cursor
            .min(meta.manifest_cursor)
            .min(meta.artifact_cursor);
        let events = self
            .journal
            .read(|tx| tx.events_since(EventSeq::new(start).ok(), 256))
            .map_err(error)?;
        for event in events {
            let seq = event.seq.ok_or_else(fail)?.get();
            let mut meta = self.journal.meta().map_err(error)?;
            let completed = matches!(&event.payload, EventPayload::JobCompleted(_));
            if seq > meta.audit_cursor {
                let head = if let Some(receipt) = self.receipts.get(&seq) {
                    receipt.clone()
                } else {
                    let action = if completed {
                        "file.finalized"
                    } else {
                        event.payload.type_name()
                    };
                    let mut entry =
                        AuditEntry::new(event.ts, Actor::engine(), action, AuditOutcome::Ok);
                    // This is an outbox observation, distinct from the write-ahead authorization record.
                    entry.write_ahead = false;
                    entry.subject.job_id = event.job_id;
                    entry.subject.run_id = event.run_id;
                    entry
                        .data
                        .insert("journal_store_id".into(), serde_json::json!(meta.store_id));
                    entry
                        .data
                        .insert("journal_seq".into(), serde_json::json!(seq));
                    entry.data.insert(
                        "event".into(),
                        serde_json::to_value(&event).map_err(|_| fail())?,
                    );
                    let record = self.audit.append(entry).map_err(error)?;
                    let head = self.audit.head().map_err(error)?.ok_or_else(fail)?;
                    AuditHead {
                        chain_id: head.chain_id,
                        seq: record.seq,
                        hash: record.hash,
                    }
                };
                self.receipts.insert(seq, head.clone());
                self.journal
                    .transact(|tx| {
                        tx.set_audit_head(Some(head))?;
                        tx.set_cursors(seq, meta.manifest_cursor)
                    })
                    .map_err(error)?;
                meta.audit_cursor = seq;
            }
            if completed {
                let id = event.job_id.ok_or_else(fail)?;
                let record = self
                    .journal
                    .read(|tx| tx.job(id))
                    .map_err(error)?
                    .ok_or_else(fail)?;
                let job = &record.job;
                let result = job.result.as_ref().ok_or_else(fail)?;
                let pointer = self.receipts.get(&seq).cloned().ok_or_else(fail)?;
                let mut draft =
                    zeus_artifact::ArtifactDraft::from_completed_job(job).map_err(error)?;
                draft.record.audit = Some(pointer.clone());
                if seq > meta.manifest_cursor {
                    let (root, writer) = self
                        .roots
                        .iter()
                        .filter(|(root, _)| Path::new(&result.path).starts_with(root))
                        .max_by_key(|(root, _)| root.components().count())
                        .ok_or_else(fail)?;
                    let zeus_artifact::ArtifactOrigin::Remote {
                        url,
                        final_url,
                        url_redacted,
                        referrer,
                        session_origin,
                        http,
                    } = &draft.record.origin
                    else {
                        return Err(fail());
                    };
                    let manifest = ManifestFile {
                        record_type: FileTag::File,
                        path: Path::new(&result.path)
                            .strip_prefix(root)
                            .map_err(|_| fail())?
                            .to_string_lossy()
                            .replace('\\', "/"),
                        size_bytes: result.size_bytes,
                        sha256: result.sha256,
                        blake3: result.blake3,
                        media_type: result.detected_mime.clone(),
                        declared_media_type: draft.record.declared_media_type.clone(),
                        source: ManifestSource {
                            kind: job.spec.source.kind,
                            url: url.clone(),
                            final_url: final_url.clone(),
                            url_redacted: *url_redacted,
                            referrer: referrer.clone(),
                        },
                        retrieved_at: draft.record.retrieved_at.unwrap_or(result.completed_at),
                        session_origin: session_origin.clone(),
                        http: http.as_deref().cloned(),
                        tools: ManifestTools {
                            zeus: env!("CARGO_PKG_VERSION").into(),
                            engine: Some(env!("CARGO_PKG_VERSION").into()),
                            extension: None,
                        },
                        converter_chain: Vec::new(),
                        license: LicenseInfo::unknown(),
                        run_id: job.spec.source.run_id,
                        job_id: id,
                        quarantined: job.quarantined,
                        os_metadata: record.internal.os_metadata.clone(),
                        derived_from: Vec::new(),
                        audit: pointer,
                    };
                    writer.append(&manifest).map_err(error)?;
                }
                if seq > meta.artifact_cursor {
                    self.artifacts
                        .ingest_record_only(
                            zeus_verify::hash::Digests {
                                sha256: result.sha256,
                                blake3: result.blake3,
                                size_bytes: result.size_bytes,
                            },
                            draft,
                            Some(&format!("zeus.job:{id}")),
                            record
                                .internal
                                .verify_result
                                .as_ref()
                                .map(|v| v.verified_at),
                        )
                        .map_err(error)?;
                }
            }
            self.journal
                .transact(|tx| {
                    tx.set_cursors(meta.audit_cursor, seq.max(meta.manifest_cursor))?;
                    tx.set_artifact_cursor(seq.max(meta.artifact_cursor))
                })
                .map_err(error)?;
            self.receipts.remove(&seq);
        }
        Ok(())
    }
}
