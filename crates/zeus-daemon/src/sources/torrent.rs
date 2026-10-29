use super::*;
use base64::Engine as _;
use std::{
    ffi::OsStr,
    io::{Read, Write},
};
use zeus_fs::DirectoryCap;
use zeus_sources::torrent::{TorrentFileReceipt, TorrentReceipt};
impl SourceService {
    pub(super) fn start_torrent(self: &Arc<Self>, row: &Operation) -> ApiResult<()> {
        let mut active = self.active.lock().map_err(|_| storage())?;
        if active.contains_key(&row.view.operation_id) {
            return Err(ApiError::new(
                "api.conflict",
                "source worker is still stopping; retry after it has stopped",
            ));
        }
        let token = tokio_util::sync::CancellationToken::new();
        active.insert(row.view.operation_id.clone(), token.clone());
        let service = self.clone();
        let row = row.clone();
        tokio::spawn(async move {
            let id = row.view.operation_id.clone();
            let result = service.acquire_torrent(&row, token).await;
            if let Err(error) = result {
                let code = error.envelope().code.to_string();
                let _ = service
                    .store
                    .mutate::<Operation, _>(&format!("operation:{id}"), |row| {
                        if row.view.state == "running" {
                            row.view.state = "failed".into();
                            row.view.error_code = Some(code);
                        }
                        Ok(())
                    });
            }
            if let Ok(mut active) = service.active.lock() {
                active.remove(&id);
            }
        });
        Ok(())
    }
    async fn acquire_torrent(
        self: &Arc<Self>,
        row: &Operation,
        cancel: tokio_util::sync::CancellationToken,
    ) -> ApiResult<()> {
        let account = self.account(&row.owner, &row.view.account_id, row.view.account_revision)?;
        let bytes = base64::engine::general_purpose::STANDARD
            .decode(row.metainfo.as_deref().ok_or_else(invalid)?)
            .map_err(|_| invalid())?;
        let review = zeus_sources::torrent::inspect(&bytes).map_err(source_error)?;
        let directory = self.state_dir.join(&row.view.operation_id);
        let authority = transport::authority(self, &row.view.operation_id);
        let receipt = zeus_sources::torrent::acquire(
            &account.view.config,
            &row.target,
            review,
            &directory,
            authority.clone(),
            cancel,
        )
        .await
        .map_err(source_error)?;
        if receipt.payload_uploaded_bytes != 0 {
            return Err(source_error(SourceError::Integrity));
        }
        let service = self.clone();
        let row = row.clone();
        tokio::task::spawn_blocking(move || {
            service.publish_torrent(&row, &directory, &receipt, authority)
        })
        .await
        .map_err(|_| storage())?
    }
    pub(super) fn publish_torrent(
        &self,
        row: &Operation,
        directory: &Path,
        receipt: &TorrentReceipt,
        authority: Arc<dyn SourceAuthority>,
    ) -> ApiResult<()> {
        let dest = self.destination(&row.owner, Some(&row.dest_dir))?;
        let destination = DirectoryCap::open_root(&dest).map_err(|_| storage())?;
        let staging = DirectoryCap::open_root(directory).map_err(|_| storage())?;
        for file in &receipt.files {
            authority.remaining().map_err(source_error)?;
            let name = row
                .request
                .torrent_filenames
                .get(&file.index)
                .cloned()
                .unwrap_or_else(|| row.filename.clone());
            let key = format!("operation:{}", row.view.operation_id);
            let existing = self
                .store
                .get::<Operation>(&key)?
                .ok_or_else(missing)?
                .publications
                .into_iter()
                .find(|p| p.file.index == file.index);
            let publication = if let Some(record) = existing {
                record
            } else {
                let temp_name = format!(
                    ".zeus-source-{}-{}.zpart",
                    row.view.operation_id, file.index
                );
                let mut source = staging
                    .open_read(OsStr::new(&format!("data-{:04}.bin", file.index)))
                    .map_err(|_| storage())?;
                let mut target = destination
                    .create_new(OsStr::new(&temp_name))
                    .map_err(|_| storage())?;
                let copied = copy_verified(&mut source, &mut target, file, authority.as_ref());
                if let Err(error) = copied {
                    let _ = destination.discard_created(OsStr::new(&temp_name), target);
                    return Err(error);
                }
                target.sync_all().map_err(|_| storage())?;
                let file_id = zeus_fs::opened_file_metadata(&target)
                    .map_err(|_| storage())?
                    .file_id;
                let publication = Publication {
                    file: SourcePublishedFile {
                        index: file.index,
                        filename: name.clone(),
                        bytes: file.bytes,
                        sha256: file.sha256.clone(),
                    },
                    temp_name,
                    file_id,
                    published: false,
                };
                self.store.mutate::<Operation, _>(&key, |op| {
                    op.publications.push(publication.clone());
                    Ok(())
                })?;
                publication
            };
            if publication.file.sha256 != file.sha256
                || publication.file.bytes != file.bytes
                || publication.file.filename != name
            {
                return Err(source_error(SourceError::Integrity));
            }
            let already_published = destination
                .entry_exists(OsStr::new(&name))
                .map_err(|_| storage())?;
            if already_published {
                let mut existing = destination
                    .open_read(OsStr::new(&name))
                    .map_err(|_| storage())?;
                let metadata = zeus_fs::opened_file_metadata(&existing).map_err(|_| storage())?;
                if metadata.file_id != publication.file_id || publication.file_id.is_none() {
                    return Err(ApiError::new(
                        "sources.destination_exists",
                        "source output destination already exists",
                    ));
                }
                verify_file(&mut existing, file, authority.as_ref())?;
            } else {
                if publication.published {
                    return Err(source_error(SourceError::Integrity));
                }
                let mut prepared = destination
                    .open_read(OsStr::new(&publication.temp_name))
                    .map_err(|_| storage())?;
                if zeus_fs::opened_file_metadata(&prepared)
                    .map_err(|_| storage())?
                    .file_id
                    != publication.file_id
                {
                    return Err(source_error(SourceError::Integrity));
                }
                verify_file(&mut prepared, file, authority.as_ref())?;
                authority.remaining().map_err(source_error)?;
                self.destination(&row.owner, Some(&row.dest_dir))?;
                destination
                    .prepare_rename(OsStr::new(&publication.temp_name))
                    .and_then(|f| f.rename_noreplace(&destination, OsStr::new(&name)))
                    .map_err(|_| storage())?;
                destination.sync().map_err(|_| storage())?;
            }
            self.store.mutate::<Operation, _>(&key, |op| {
                let entry = op
                    .publications
                    .iter_mut()
                    .find(|p| p.file.index == file.index)
                    .ok_or_else(storage)?;
                entry.published = true;
                Ok(())
            })?;
        }
        self.store
            .mutate::<Operation, _>(&format!("operation:{}", row.view.operation_id), |op| {
                if op.view.state != "running"
                    || op.publications.len() != receipt.files.len()
                    || op.publications.iter().any(|p| !p.published)
                {
                    return Err(denied());
                }
                op.view.files = op.publications.iter().map(|p| p.file.clone()).collect();
                op.view.payload_uploaded_bytes = receipt.payload_uploaded_bytes;
                op.view.output_sha256 =
                    (op.view.files.len() == 1).then(|| op.view.files[0].sha256.clone());
                op.view.state = "completed".into();
                op.view.error_code = None;
                Ok(())
            })
    }
}
fn copy_verified(
    source: &mut std::fs::File,
    target: &mut std::fs::File,
    file: &TorrentFileReceipt,
    authority: &dyn SourceAuthority,
) -> ApiResult<()> {
    let mut hash = zeus_verify::DualHasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        authority.remaining().map_err(source_error)?;
        let count = source.read(&mut buffer).map_err(|_| storage())?;
        if count == 0 {
            break;
        }
        size = size.checked_add(count as u64).ok_or_else(invalid)?;
        if size > file.bytes {
            return Err(source_error(SourceError::Integrity));
        }
        hash.update(&buffer[..count]);
        target.write_all(&buffer[..count]).map_err(|_| storage())?;
    }
    if size != file.bytes || hash.finalize().sha256.to_string() != file.sha256 {
        return Err(source_error(SourceError::Integrity));
    }
    Ok(())
}
fn verify_file(
    source: &mut std::fs::File,
    file: &TorrentFileReceipt,
    authority: &dyn SourceAuthority,
) -> ApiResult<()> {
    let mut hash = zeus_verify::DualHasher::new();
    let mut buffer = [0u8; 64 * 1024];
    let mut size = 0u64;
    loop {
        authority.remaining().map_err(source_error)?;
        let count = source.read(&mut buffer).map_err(|_| storage())?;
        if count == 0 {
            break;
        }
        size = size.checked_add(count as u64).ok_or_else(invalid)?;
        if size > file.bytes {
            return Err(source_error(SourceError::Integrity));
        }
        hash.update(&buffer[..count]);
    }
    if size != file.bytes || hash.finalize().sha256.to_string() != file.sha256 {
        return Err(source_error(SourceError::Integrity));
    }
    Ok(())
}
