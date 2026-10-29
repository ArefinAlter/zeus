//! `artifacts.get`, `artifacts.list` and `artifacts.lineage` (05 section 9.13): export forms, scoped like jobs.
use super::*;
use std::collections::{BTreeMap, VecDeque};
use zeus_artifact::{Artifact, ArtifactFilter, ArtifactState, ParentRef, Privacy};
use zeus_verify::provenance::UrlQueryPrivacy;

/// Records one listing considers (the store's query bound).
const LIST_BOUND: usize = 10_000;

/// The API export: URL queries dropped and session origins withheld for every caller (13 section 14).
fn export_privacy() -> Privacy {
    Privacy {
        url_query: UrlQueryPrivacy::Dropped,
        session_origin_included: false,
    }
}

/// A parent reference resolves to the record it names, or by content to every equal record (13 section 4.3).
fn resolves(parent: &ParentRef, record: &Artifact) -> bool {
    match parent.artifact_id {
        Some(id) => id == record.id,
        None => parent.blake3 == record.blake3 && parent.size_bytes == record.size_bytes,
    }
}

fn push_link(links: &mut Vec<(usize, String)>, node: usize, role: &str) {
    if !links.iter().any(|(n, r)| *n == node && r == role) {
        links.push((node, role.to_owned()));
    }
}

impl RuntimeBackend {
    /// Services see the records of their own jobs and those they ingested (05 section 9.13); everyone else sees
    /// every record.
    pub(super) fn artifact_visible(&self, ctx: &CallContext, record: &Artifact) -> bool {
        let scope = Self::scope(ctx);
        if !record.imports.is_empty() {
            return scope.service().is_none()
                || scope
                    .service()
                    .is_some_and(|me| self.ingest_owners.owner(record.id).as_deref() == Some(me));
        }
        scope.service().is_none()
            || record
                .job_id
                .is_some_and(|id| scope.job_id(id, &self.journal))
            || scope
                .service()
                .is_some_and(|me| self.ingest_owners.owner(record.id).as_deref() == Some(me))
    }

    /// The record, or `artifact.not_found` when it is unknown or outside the caller's view.
    pub(super) fn visible_artifact(
        &self,
        ctx: &CallContext,
        id: zeus_types::ArtifactId,
    ) -> ApiResult<Artifact> {
        let record = self.artifacts.get(id).map_err(error)?;
        if !self.artifact_visible(ctx, &record) {
            return Err(error(zeus_artifact::ArtifactError::NotFound));
        }
        Ok(record)
    }

    pub(super) fn export_value(&self, id: zeus_types::ArtifactId) -> ApiResult<serde_json::Value> {
        let record = self
            .artifacts
            .export(id, &export_privacy())
            .map_err(error)?;
        serde_json::to_value(record).map_err(|_| internal())
    }

    pub(super) fn artifact_get(
        &self,
        ctx: &CallContext,
        p: ArtifactsGetParams,
    ) -> ApiResult<ArtifactsGetResult> {
        let record = self.visible_artifact(ctx, p.artifact_id)?;
        Ok(ArtifactsGetResult {
            artifact: self.export_value(record.id)?,
        })
    }

    /// The newest [`LIST_BOUND`] records the caller may see and the filters match, newest first.
    pub(super) fn artifact_list(
        &self,
        ctx: &CallContext,
        p: ArtifactsListParams,
    ) -> ApiResult<ArtifactsListResult> {
        p.page.validate()?;
        let ids: Vec<_> = self
            .artifacts
            .list_newest_where(
                &ArtifactFilter {
                    blake3: p.blake3,
                    job_id: p.job_id,
                    run_id: p.run_id,
                    include_deleted: p.state != Some(ArtifactStateFilter::Active),
                    limit: LIST_BOUND,
                },
                |r| {
                    p.sha256.is_none_or(|d| r.sha256 == d)
                        && (p.state != Some(ArtifactStateFilter::Deleted)
                            || r.state == ArtifactState::Deleted)
                        && self.artifact_visible(ctx, r)
                },
            )
            .map_err(error)?
            .into_iter()
            .map(|r| r.id)
            .collect();
        let ids = page(ids, &p.page)?;
        Ok(Page {
            items: ids
                .items
                .into_iter()
                .map(|id| self.export_value(id))
                .collect::<ApiResult<_>>()?,
            next_cursor: ids.next_cursor,
        })
    }

    /// A bounded walk from the start record. Records outside the caller's view, and records reachable only
    /// through them, are omitted and reported as `truncated`; a parent with no visible record becomes a
    /// content-only node, exactly as if it had never been recorded.
    pub(super) fn artifact_lineage(
        &self,
        ctx: &CallContext,
        p: ArtifactsLineageParams,
    ) -> ApiResult<ArtifactsLineageResult> {
        let max_depth = p.max_depth.unwrap_or(8);
        let max_nodes = p.max_nodes.unwrap_or(100) as usize;
        if !(1..=64).contains(&max_depth) || !(1..=500).contains(&max_nodes) {
            return Err(ApiError::invalid_params(
                "max_depth is 1 to 64 and max_nodes 1 to 500",
            ));
        }
        self.visible_artifact(ctx, p.artifact_id)?;
        let descendants = p.direction == LineageDirection::Descendants;
        let walk = self
            .artifacts
            .lineage(p.artifact_id, descendants, max_depth as usize, max_nodes)
            .map_err(error)?;
        let mut truncated = walk.truncated;
        let records: Vec<&Artifact> = walk
            .artifacts
            .iter()
            .filter(|r| self.artifact_visible(ctx, r))
            .collect();
        truncated |= records.len() < walk.artifacts.len();

        // Edges point away from the start record: to parents (ancestors) or to children (descendants).
        let mut edges: Vec<Vec<(usize, String)>> = vec![Vec::new(); records.len()];
        for (i, record) in records.iter().enumerate() {
            for (j, other) in records.iter().enumerate().filter(|(j, _)| *j != i) {
                let (child, parent_node) = if descendants {
                    (other, record)
                } else {
                    (record, other)
                };
                for parent in child.parents.iter().filter(|p| resolves(p, parent_node)) {
                    push_link(&mut edges[i], j, &parent.role);
                }
            }
        }
        let mut depth: Vec<Option<u32>> = vec![None; records.len()];
        depth[0] = Some(0);
        let mut queue = VecDeque::from([0usize]);
        while let Some(i) = queue.pop_front() {
            let next = depth[i].unwrap_or(0) + 1;
            for &(j, _) in &edges[i] {
                if depth[j].is_none() && next <= max_depth {
                    depth[j] = Some(next);
                    queue.push_back(j);
                }
            }
        }
        let kept: Vec<usize> = (0..records.len()).filter(|&i| depth[i].is_some()).collect();
        truncated |= kept.len() < records.len();
        let index: BTreeMap<usize, u32> = kept
            .iter()
            .enumerate()
            .map(|(new, &old)| (old, new as u32))
            .collect();
        let mut nodes: Vec<LineageNode> = kept
            .iter()
            .map(|&i| LineageNode {
                artifact_id: Some(records[i].id),
                blake3: records[i].blake3,
                size_bytes: records[i].size_bytes,
                depth: depth[i].unwrap_or(0),
                links: edges[i]
                    .iter()
                    .filter_map(|(j, role)| {
                        index.get(j).map(|&node| LineageLink {
                            node,
                            role: role.clone(),
                        })
                    })
                    .collect(),
            })
            .collect();

        if !descendants {
            // Parents not linked to a returned record: omitted by a bound, or content-only.
            let mut content_only: BTreeMap<(zeus_types::Hex256, u64), u32> = BTreeMap::new();
            for (position, &i) in kept.iter().enumerate() {
                let record = records[i];
                for parent in &record.parents {
                    if kept.iter().any(|&k| k != i && resolves(parent, records[k])) {
                        continue;
                    }
                    let recorded = match parent.artifact_id {
                        Some(id) => self
                            .artifacts
                            .get(id)
                            .is_ok_and(|r| self.artifact_visible(ctx, &r)),
                        None => self
                            .artifacts
                            .list(&ArtifactFilter {
                                blake3: Some(parent.blake3),
                                include_deleted: true,
                                limit: LIST_BOUND,
                                ..Default::default()
                            })
                            .map_err(error)?
                            .iter()
                            .any(|r| {
                                r.id != record.id
                                    && r.size_bytes == parent.size_bytes
                                    && self.artifact_visible(ctx, r)
                            }),
                    };
                    if parent.artifact_id.is_none() && resolves(parent, record) && !recorded {
                        continue; // an identity operation: its only match is the record itself
                    }
                    let parent_depth = nodes[position].depth + 1;
                    if recorded || parent_depth > max_depth {
                        truncated = true;
                        continue;
                    }
                    let key = (parent.blake3, parent.size_bytes);
                    let node = match content_only.get(&key) {
                        Some(&node) => node,
                        None if nodes.len() < max_nodes => {
                            let node = nodes.len() as u32;
                            nodes.push(LineageNode {
                                artifact_id: None,
                                blake3: parent.blake3,
                                size_bytes: parent.size_bytes,
                                depth: parent_depth,
                                links: Vec::new(),
                            });
                            content_only.insert(key, node);
                            node
                        }
                        None => {
                            truncated = true;
                            continue;
                        }
                    };
                    let links = &mut nodes[position].links;
                    if !links
                        .iter()
                        .any(|l| l.node == node && l.role == parent.role)
                    {
                        links.push(LineageLink {
                            node,
                            role: parent.role.clone(),
                        });
                    }
                }
            }
        }
        Ok(ArtifactsLineageResult { nodes, truncated })
    }
}
