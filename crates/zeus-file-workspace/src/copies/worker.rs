use super::*;
use std::io::{Read, Seek, SeekFrom, Write};

fn deadline(start: Instant) -> Result<()> {
    if start.elapsed() > VERIFY_TIME {
        Err(failed(
            "Copy scan or checkpoint verification exceeded its two-minute cooperative limit. Its checkpoint is held for review.",
        ))
    } else {
        Ok(())
    }
}
fn checked_source(
    authority: &Authority,
    plan: &Plan,
    entry: &Entry,
) -> Result<(DirectoryCap, std::ffi::OsString)> {
    let path = Path::new(&plan.source).join(&entry.relative);
    let (parent, name) = authority.reads.parent(&path)?;
    if parent.file_id().map_err(cap_failure)? != entry.parent_id
        || entry_stamp(&parent, &name)? != entry.stamp
    {
        return Err(failed(
            "The original source or its parent changed. This receipt cannot adopt new content.",
        ));
    }
    Ok((parent, name))
}
pub(super) fn check_original(authority: &Authority, plan: &Plan, entry: &Entry) -> Result<()> {
    checked_source(authority, plan, entry).map(|_| ())
}
fn bindings(authority: &Authority, plan: &Plan) -> Result<DirectoryCap> {
    (authority.check)()?;
    let current_reads = authority.reads.bindings()?;
    let current_writes = authority.writes.bindings()?;
    if !plan.reads.iter().all(|p| current_reads.contains(p))
        || !plan.writes.iter().all(|p| current_writes.contains(p))
    {
        return Err(failed(
            "A recorded copy root is no longer enabled with its original identity.",
        ));
    }
    let destination = authority.writes.directory(Path::new(&plan.destination))?;
    if destination.file_id().map_err(cap_failure)? != plan.destination_id {
        return Err(failed(
            "The destination folder was replaced. Its new identity is not authorized by this receipt.",
        ));
    }
    (authority.path_check)(&plan.source)?;
    (authority.path_check)(&shown(&plan.target()))?;
    Ok(destination)
}
pub(super) fn plan(from: &str, to: &str, authority: &Authority) -> Result<Plan> {
    plan_with_target(from, to, authority, false)
}
pub(super) fn plan_with_target(
    from: &str,
    to: &str,
    authority: &Authority,
    allow_occupied: bool,
) -> Result<Plan> {
    (authority.check)()?;
    let start = Instant::now();
    let source = authority.reads.path(from)?;
    let destination = authority.writes.path(to)?;
    if destination.starts_with(&source) {
        return Err(Failure::invalid(
            "Choose an existing destination outside the source.",
        ));
    }
    let cap = authority.writes.directory(&destination)?;
    let name = source
        .file_name()
        .and_then(|s| s.to_str())
        .ok_or_else(|| failed("Choose a file or folder inside an enabled root."))?
        .to_owned();
    if !allow_occupied && cap.entry_exists(name.as_ref()).map_err(cap_failure)? {
        return Err(Failure::new(
            "app.exists",
            "The destination already exists. Nothing was replaced.",
        ));
    }
    (authority.path_check)(&shown(&source))?;
    (authority.path_check)(&shown(&destination.join(&name)))?;
    let mut pending = vec![source.clone()];
    let mut entries = Vec::new();
    let mut bytes = 0u64;
    while let Some(path) = pending.pop() {
        deadline(start)?;
        (authority.check)()?;
        let relative = path
            .strip_prefix(&source)
            .map_err(|_| failed("Invalid source path."))?;
        if relative.components().count() > 128 || entries.len() + pending.len() >= MAX_ENTRIES {
            return Err(failed(
                "Copy at most 100,000 entries and 128 folder levels at a time.",
            ));
        }
        let (parent, leaf) = authority.reads.parent(&path)?;
        let stamp = entry_stamp(&parent, &leaf)?;
        if stamp.directory {
            let dir = parent.open_dir(Path::new(&leaf)).map_err(cap_failure)?;
            if dir.file_id().map_err(cap_failure)? != stamp.id {
                return Err(failed("A source directory changed during the scan."));
            }
            let mut names = dir
                .entry_names(MAX_ENTRIES - entries.len() - pending.len())
                .map_err(cap_failure)?;
            names.sort();
            for name in names.into_iter().rev() {
                if name == ".zeus" || name == ".zeus-partial" {
                    return Err(failed(
                        "This folder contains Zeus recovery storage. Choose ordinary child folders so private receipts and staging are not copied.",
                    ));
                }
                pending.push(path.join(name));
            }
        } else {
            bytes = bytes
                .checked_add(stamp.size)
                .filter(|n| *n <= MAX_JOB_BYTES)
                .ok_or_else(|| failed("One copy can reserve at most 256 GiB of staging space."))?;
        }
        entries.push(Entry {
            relative: utf8(relative)?,
            stamp,
            parent_id: parent.file_id().map_err(cap_failure)?,
            content_sha256: None,
        });
    }
    if cap.free_space().map_err(cap_failure)? < bytes.saturating_add(16 * 1024 * 1024) {
        return Err(failed(
            "The destination does not currently have enough free space for the copy and its checkpoint.",
        ));
    }
    let plan = Plan {
        id: crate::random_id(),
        source: shown(&source),
        destination: shown(&destination),
        name,
        reads: vec![authority.reads.binding_for(&source)?],
        writes: vec![authority.writes.binding_for(&destination)?],
        destination_id: cap.file_id().map_err(cap_failure)?,
        entries,
        bytes,
    };
    plan.validate()?;
    if serde_json::to_vec(&plan)?.len() > 64 * 1024 * 1024 {
        return Err(failed("The copy manifest exceeds its 64 MiB bound."));
    }
    Ok(plan)
}
pub(super) fn pin_content(plan: &mut Plan, authority: &Authority, start: Instant) -> Result<()> {
    let frozen = plan.clone();
    let mut buffer = vec![0; 1024 * 1024];
    for entry in &mut plan.entries {
        deadline(start)?;
        (authority.check)()?;
        let (parent, name) = checked_source(authority, &frozen, entry)?;
        if entry.stamp.directory {
            continue;
        }
        let mut file = parent.open_read(&name).map_err(cap_failure)?;
        if open_stamp(&file)? != entry.stamp {
            return Err(failed("The source changed before content review."));
        }
        let mut digest = Sha256::new();
        let mut remaining = entry.stamp.size;
        while remaining > 0 {
            deadline(start)?;
            (authority.check)()?;
            let take = remaining.min(buffer.len() as u64) as usize;
            file.read_exact(&mut buffer[..take])?;
            digest.update(&buffer[..take]);
            remaining -= take as u64;
        }
        if open_stamp(&file)? != entry.stamp {
            return Err(failed("The source changed during content review."));
        }
        entry.content_sha256 = Some(hex::encode(digest.finalize()));
    }
    for entry in &plan.entries {
        deadline(start)?;
        (authority.check)()?;
        checked_source(authority, plan, entry)?;
    }
    Ok(())
}

fn stage_parent(
    stage: &DirectoryCap,
    entry: &Entry,
    plan: &Plan,
    progress: &[Progress],
) -> Result<(DirectoryCap, std::ffi::OsString)> {
    let rel = if entry.relative.is_empty() {
        PathBuf::from("payload")
    } else {
        Path::new("payload").join(&entry.relative)
    };
    let parent = stage
        .open_dir(rel.parent().unwrap_or_else(|| Path::new("")))
        .map_err(cap_failure)?;
    if !entry.relative.is_empty() {
        let relative_parent = Path::new(&entry.relative)
            .parent()
            .unwrap_or_else(|| Path::new(""));
        let index = plan
            .entries
            .iter()
            .position(|e| Path::new(&e.relative) == relative_parent)
            .ok_or_else(|| failed("A staging parent is absent from the manifest."))?;
        if Some(parent.file_id().map_err(cap_failure)?) != progress[index].id {
            return Err(failed("A staging parent was replaced."));
        }
    }
    Ok((parent, rel.file_name().unwrap().to_owned()))
}
fn verify_chunk(
    file: &mut std::fs::File,
    size: usize,
    digest: &str,
    buffer: &mut [u8],
) -> Result<()> {
    file.read_exact(&mut buffer[..size])?;
    if hex::encode(Sha256::digest(&buffer[..size])) != digest {
        return Err(failed(
            "Recorded copy bytes no longer match their SHA-256 checkpoint. Both copies were preserved.",
        ));
    }
    Ok(())
}
fn progress_snapshot(job: &Job) -> Vec<Progress> {
    job.receipt.lock().unwrap().progress.clone()
}
pub(super) fn verify_sources(
    job: &Job,
    authority: &Authority,
    plan: &Plan,
    start: Instant,
) -> Result<()> {
    for entry in &plan.entries {
        deadline(start)?;
        job.check(authority)?;
        checked_source(authority, plan, entry)?;
    }
    Ok(())
}
// Verify exact recorded names as well as identities: an injected extra child is never published.
fn verify_tree_shape(
    stage: &DirectoryCap,
    plan: &Plan,
    progress: &[Progress],
    published: bool,
) -> Result<()> {
    let mut expected: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (i, entry) in plan.entries.iter().enumerate() {
        if progress[i].id.is_none() {
            continue;
        }
        let relative = if published {
            PathBuf::from(&entry.relative)
        } else {
            if entry.relative.is_empty() {
                PathBuf::from("payload")
            } else {
                Path::new("payload").join(&entry.relative)
            }
        };
        if relative.as_os_str().is_empty() {
            continue;
        }
        let parent = utf8(relative.parent().unwrap_or_else(|| Path::new("")))?;
        expected.entry(parent).or_default().push(
            relative
                .file_name()
                .and_then(|s| s.to_str())
                .ok_or_else(|| failed("An output filename is invalid."))?
                .into(),
        );
        if entry.stamp.directory {
            expected.entry(utf8(&relative)?).or_default();
        }
    }
    if !published {
        expected.entry(String::new()).or_default();
    }
    for (path, mut names) in expected {
        names.sort();
        let dir = stage.open_dir(Path::new(&path)).map_err(cap_failure)?;
        let mut actual = dir
            .entry_names(MAX_ENTRIES)
            .map_err(cap_failure)?
            .into_iter()
            .map(|s| {
                s.into_string()
                    .map_err(|_| failed("An unexpected staging entry was found."))
            })
            .collect::<Result<Vec<_>>>()?;
        actual.sort();
        if actual != names {
            return Err(failed(
                "Private staging contains missing or unexpected entries. Nothing was published.",
            ));
        }
    }
    Ok(())
}
fn verify_progress(
    job: &Job,
    authority: &Authority,
    plan: &Plan,
    stage: &DirectoryCap,
    start: Instant,
) -> Result<()> {
    let progress = progress_snapshot(job);
    verify_tree_shape(stage, plan, &progress, false)?;
    let mut source_buffer = vec![0; CHUNK];
    let mut target_buffer = vec![0; CHUNK];
    for (index, entry) in plan.entries.iter().enumerate() {
        deadline(start)?;
        job.check(authority)?;
        let (source_parent, source_name) = checked_source(authority, plan, entry)?;
        let p = &progress[index];
        let Some(id) = &p.id else {
            continue;
        };
        let (parent, name) = stage_parent(stage, entry, plan, &progress)?;
        let observed = parent.entry_metadata(&name).map_err(cap_failure)?;
        if observed.file_id.as_ref() != Some(id) || observed.is_dir != entry.stamp.directory {
            return Err(failed("A private staging object was replaced."));
        }
        if entry.stamp.directory {
            continue;
        }
        let mut input = source_parent.open_read(&source_name).map_err(cap_failure)?;
        if open_stamp(&input)? != entry.stamp {
            return Err(failed("The source changed before resume verification."));
        }
        let mut output = parent.open_existing(&name).map_err(cap_failure)?;
        if file_id(&output)? != *id {
            return Err(failed("The staging file changed before verification."));
        }
        let committed = (p.chunks.len() as u64 * CHUNK as u64).min(entry.stamp.size);
        if output.metadata()?.len() < committed {
            return Err(failed("The staging file lost committed bytes."));
        }
        let mut content_digest = Sha256::new();
        for (chunk, digest) in p.chunks.iter().enumerate() {
            deadline(start)?;
            job.check(authority)?;
            let size = (entry.stamp.size - chunk as u64 * CHUNK as u64).min(CHUNK as u64) as usize;
            verify_chunk(&mut input, size, digest, &mut source_buffer)?;
            content_digest.update(&source_buffer[..size]);
            verify_chunk(&mut output, size, digest, &mut target_buffer)?;
        }
        if open_stamp(&input)? != entry.stamp {
            return Err(failed(
                "The original source changed during resume verification.",
            ));
        }
        if committed == entry.stamp.size
            && entry
                .content_sha256
                .as_ref()
                .is_some_and(|expected| *expected != hex::encode(content_digest.finalize()))
        {
            return Err(failed(
                "The source bytes differ from the exact approved copy manifest. Nothing was published.",
            ));
        }
        // A write may have reached disk before its receipt; only the verified prefix is committed.
        if p.modified.is_none() && output.metadata()?.len() != committed {
            output.set_len(committed)?;
            output.sync_all()?;
        }
        if let Some(modified) = p.modified
            && (output.metadata()?.len() != entry.stamp.size
                || output.metadata()?.modified()? != modified)
        {
            return Err(failed(
                "A completed staging file changed after its checkpoint.",
            ));
        }
    }
    Ok(())
}
fn recover_published(
    job: &Job,
    log: &mut Log,
    authority: &Authority,
    plan: &Plan,
    destination: &DirectoryCap,
    start: Instant,
) -> Result<()> {
    let progress = progress_snapshot(job);
    let root = &plan.entries[0];
    let observed = destination
        .entry_metadata(plan.name.as_ref())
        .map_err(cap_failure)?;
    if observed.file_id.as_ref() != progress[0].id.as_ref() {
        return Err(failed(
            "The publication destination is a different object. It was preserved.",
        ));
    }
    let tree = if root.stamp.directory {
        Some(
            destination
                .open_dir(Path::new(&plan.name))
                .map_err(cap_failure)?,
        )
    } else {
        None
    };
    if let Some(tree) = &tree {
        verify_tree_shape(tree, plan, &progress, true)?;
    }
    let mut buffer = vec![0; CHUNK];
    for (index, entry) in plan.entries.iter().enumerate() {
        deadline(start)?;
        job.check(authority)?;
        let (source_parent, source_name) = checked_source(authority, plan, entry)?;
        let path = Path::new(&plan.name).join(&entry.relative);
        if entry.stamp.directory {
            let dir = destination.open_dir(&path).map_err(cap_failure)?;
            if Some(dir.file_id().map_err(cap_failure)?) != progress[index].id {
                return Err(failed("A published directory was replaced."));
            }
            continue;
        }
        let parent = destination
            .open_dir(path.parent().unwrap())
            .map_err(cap_failure)?;
        let mut input = parent
            .open_read(path.file_name().unwrap())
            .map_err(cap_failure)?;
        if file_id(&input)?
            != progress[index]
                .id
                .clone()
                .ok_or_else(|| failed("A publication receipt is incomplete."))?
            || input.metadata()?.len() != entry.stamp.size
        {
            return Err(failed("A published file changed before reconciliation."));
        }
        let before = input.metadata()?.modified()?;
        let mut original = source_parent.open_read(&source_name).map_err(cap_failure)?;
        if open_stamp(&original)? != entry.stamp {
            return Err(failed(
                "The original source changed before publication reconciliation.",
            ));
        }
        let mut source_buffer = vec![0; CHUNK];
        for (chunk, digest) in progress[index].chunks.iter().enumerate() {
            deadline(start)?;
            job.check(authority)?;
            let size = (entry.stamp.size - chunk as u64 * CHUNK as u64).min(CHUNK as u64) as usize;
            verify_chunk(&mut input, size, digest, &mut buffer)?;
            verify_chunk(&mut original, size, digest, &mut source_buffer)?;
        }
        if open_stamp(&original)? != entry.stamp {
            return Err(failed(
                "The original source changed during publication reconciliation.",
            ));
        }
        if input.metadata()?.modified()? != before {
            return Err(failed("A published file changed during reconciliation."));
        }
    }
    job.event(log, Event::Complete)
}
pub(super) fn run(job: &Job, log: &mut Log, authority: &Authority) -> Result<()> {
    let start = Instant::now();
    job.check(authority)?;
    let (plan, stage_id, publishing) = {
        let r = job.receipt.lock().unwrap();
        (r.plan.clone(), r.stage_id.clone(), r.publishing)
    };
    let destination = bindings(authority, &plan)?;
    verify_sources(job, authority, &plan, start)?;
    if publishing
        && destination
            .entry_exists(plan.name.as_ref())
            .map_err(cap_failure)?
    {
        return recover_published(job, log, authority, &plan, &destination, start);
    }
    if destination
        .entry_exists(plan.name.as_ref())
        .map_err(cap_failure)?
    {
        return Err(Failure::new(
            "app.exists",
            "The destination now exists. The staging copy is preserved.",
        ));
    }
    let private = destination
        .create_dir_all(Path::new(".zeus-partial"))
        .map_err(cap_failure)?;
    let stage_name = plan.stage();
    let stage = if let Some(id) = stage_id {
        let stage = private
            .open_dir(Path::new(&stage_name))
            .map_err(cap_failure)?;
        if stage.file_id().map_err(cap_failure)? != id {
            return Err(failed(
                "Private copy staging was replaced. No new authority was adopted.",
            ));
        }
        stage
    } else {
        private
            .create_dir(stage_name.as_ref())
            .map_err(cap_failure)?;
        private.sync().map_err(cap_failure)?;
        let stage = private
            .open_dir(Path::new(&stage_name))
            .map_err(cap_failure)?;
        job.event(
            log,
            Event::Stage {
                identity: stage.file_id().map_err(cap_failure)?,
            },
        )?;
        stage
    };
    verify_progress(job, authority, &plan, &stage, start)?;
    if !publishing {
        job.receipt.lock().unwrap().state = "copying".into();
        let mut buffer = vec![0; CHUNK];
        for (index, entry) in plan.entries.iter().enumerate() {
            job.check(authority)?;
            let (source_parent, source_name) = checked_source(authority, &plan, entry)?;
            let p = job.receipt.lock().unwrap().progress[index].clone();
            if p.modified.is_some() {
                continue;
            }
            let (parent, name) = stage_parent(&stage, entry, &plan, &progress_snapshot(job))?;
            if entry.stamp.directory {
                if p.id.is_none() {
                    parent.create_dir(&name).map_err(cap_failure)?;
                    parent.sync().map_err(cap_failure)?;
                    let id = parent
                        .entry_metadata(&name)
                        .map_err(cap_failure)?
                        .file_id
                        .ok_or_else(|| failed("A created directory has no stable identity."))?;
                    job.event(
                        log,
                        Event::Created {
                            index,
                            identity: id,
                        },
                    )?;
                }
                let modified = parent
                    .entry_os_metadata(&name)
                    .map_err(cap_failure)?
                    .modified()?;
                job.event(log, Event::Done { index, modified })?;
                continue;
            }
            let mut input = source_parent.open_read(&source_name).map_err(cap_failure)?;
            if open_stamp(&input)? != entry.stamp {
                return Err(failed("The source changed before copying."));
            }
            let mut output = if let Some(id) = p.id {
                let file = parent.open_existing(&name).map_err(cap_failure)?;
                if file_id(&file)? != id {
                    return Err(failed("The staging file was replaced."));
                }
                file
            } else {
                let file = parent.create_new(&name).map_err(cap_failure)?;
                file.sync_all()?;
                parent.sync().map_err(cap_failure)?;
                job.event(
                    log,
                    Event::Created {
                        index,
                        identity: file_id(&file)?,
                    },
                )?;
                file
            };
            let mut offset = (p.chunks.len() as u64 * CHUNK as u64).min(entry.stamp.size);
            input.seek(SeekFrom::Start(offset))?;
            output.seek(SeekFrom::Start(offset))?;
            while offset < entry.stamp.size {
                job.check(authority)?;
                let count = (entry.stamp.size - offset).min(CHUNK as u64) as usize;
                input.read_exact(&mut buffer[..count])?;
                if open_stamp(&input)? != entry.stamp {
                    return Err(failed(
                        "The source changed while copying. No new bytes were checkpointed.",
                    ));
                }
                output.write_all(&buffer[..count])?;
                output.sync_all()?;
                job.event(
                    log,
                    Event::Chunk {
                        index,
                        digest: hex::encode(Sha256::digest(&buffer[..count])),
                    },
                )?;
                offset += count as u64;
            }
            if open_stamp(&input)? != entry.stamp {
                return Err(failed("The source changed at the end of copying."));
            }
            output.sync_all()?;
            job.event(
                log,
                Event::Done {
                    index,
                    modified: output.metadata()?.modified()?,
                },
            )?;
        }
    }
    // A fresh shared deadline bounds final revalidation, independently of transfer duration.
    let final_start = Instant::now();
    verify_sources(job, authority, &plan, final_start)?;
    verify_progress(job, authority, &plan, &stage, final_start)?;
    bindings(authority, &plan)?;
    job.check(authority)?;
    if !publishing {
        job.event(log, Event::Publishing)?;
    }
    bindings(authority, &plan)?;
    job.check(authority)?;
    let root_id = job.receipt.lock().unwrap().progress[0]
        .id
        .clone()
        .ok_or_else(|| failed("Publication has no bound root identity."))?;
    let source = stage
        .prepare_rename("payload".as_ref())
        .map_err(cap_failure)?;
    if source.metadata().map_err(cap_failure)?.file_id.as_ref() != Some(&root_id) {
        return Err(failed(
            "The completed staging root changed before publication.",
        ));
    }
    source
        .rename_noreplace(&destination, plan.name.as_ref())
        .map_err(cap_failure)?;
    destination.sync().map_err(cap_failure)?;
    stage.sync().map_err(cap_failure)?;
    job.event(log, Event::Complete)
}
