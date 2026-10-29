//! Opt-in local stream-copy muxing from completed, hash-verified media tasks.
//! No arbitrary path, filter, codec, command line, URL or shell is accepted.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use zeus_app::failure::{Failure, Result};
use zeus_fs::provenance::{NativeMarker, ProvenanceMarker, ProvenanceMarks};
use zeus_fs::{DataFile, Filesystem, RealFs, SafeRoot};

use super::http::canceled;
use super::runner::{self, Pin};
use super::transfer::{self, Checkpoint, Progress, Receipt};

/// Largest total set of verified input files accepted by the mux plan.
pub(crate) const INPUT_BYTES: u64 = 100 * 1024 * 1024 * 1024;

fn io_error(_: zeus_fs::FsError) -> Failure {
    Failure::new(
        "media.filesystem",
        "The media conversion file operation failed. Check the configured folder and free disk space.",
    )
}

fn demuxer(filename: &str) -> Result<&'static str> {
    match filename
        .rsplit('.')
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
        .as_str()
    {
        "mp4" | "m4a" | "mov" => Ok("mov"),
        "ts" => Ok("mpegts"),
        "mkv" | "webm" => Ok("matroska"),
        "mp3" => Ok("mp3"),
        "aac" => Ok("aac"),
        "flac" => Ok("flac"),
        "ogg" | "opus" => Ok("ogg"),
        _ => Err(Failure::new(
            "media.mux_input_unsupported",
            "This source container is not supported by the fixed stream-copy mux profile.",
        )),
    }
}

/// Validate only the stream-copy input formats supported by the closed profile.
pub(crate) fn validate_input(filename: &str) -> Result<()> {
    demuxer(filename).map(|_| ())
}

/// Fixed FFmpeg arguments. Input demuxers and output muxer are selected from
/// closed enums. HTTP, playlists, devices, arbitrary filters and shell syntax
/// cannot be requested through this interface.
fn arguments(inputs: &[LocalInput], container: &str, aac_framing: bool) -> Result<Vec<OsString>> {
    if inputs.is_empty() || inputs.len() > 2 || !matches!(container, "mp4" | "mkv") {
        return Err(Failure::invalid(
            "Mux one media file or a video/audio pair into MP4 or MKV.",
        ));
    }
    let mut args: Vec<OsString> = [
        "-nostdin",
        "-hide_banner",
        "-nostats",
        "-loglevel",
        "error",
        "-max_alloc",
        "67108864",
        "-n",
    ]
    .into_iter()
    .map(OsString::from)
    .collect();
    for input in inputs {
        let format = input.format;
        args.extend(
            ["-protocol_whitelist", "file", "-f", format]
                .into_iter()
                .map(OsString::from),
        );
        if format == "mov" {
            args.extend(
                ["-enable_drefs", "0", "-use_absolute_path", "0"]
                    .into_iter()
                    .map(OsString::from),
            );
        }
        args.push("-i".into());
        args.push(input.path.clone().into_os_string());
    }
    if inputs.len() == 2 {
        args.extend(
            ["-map", "0:v:0", "-map", "1:a:0"]
                .into_iter()
                .map(OsString::from),
        );
    } else {
        args.extend(
            ["-map", "0:v:0?", "-map", "0:a:0?"]
                .into_iter()
                .map(OsString::from),
        );
    }
    args.extend(
        [
            "-map_metadata",
            "-1",
            "-map_chapters",
            "-1",
            "-sn",
            "-dn",
            "-c",
            "copy",
            "-threads",
            "2",
        ]
        .into_iter()
        .map(OsString::from),
    );
    if aac_framing {
        // This closed fallback changes AAC framing, never its encoding. Try
        // ordinary copy first so non-AAC TS audio remains supported as well.
        args.extend(["-bsf:a", "aac_adtstoasc"].into_iter().map(OsString::from));
    }
    if container == "mp4" {
        args.extend(
            [
                "-movflags",
                "+frag_keyframe+delay_moov+default_base_moof",
                "-frag_duration",
                "2000000",
                "-frag_size",
                "16777216",
                "-f",
                "mp4",
            ]
            .into_iter()
            .map(OsString::from),
        );
    } else {
        args.extend(["-f", "matroska"].into_iter().map(OsString::from));
    }
    args.push("pipe:1".into());
    Ok(args)
}

fn copy_input(
    source: Checkpoint,
    index: usize,
    directory: PathBuf,
    progress: Arc<Progress>,
    deadline: Instant,
) -> Result<Receipt> {
    let root = SafeRoot::open(&source.root).map_err(io_error)?;
    let path = root
        .join_checked(Path::new(&source.filename))
        .map_err(io_error)?;
    root.check_existing(&path).map_err(io_error)?;
    let input = RealFs.open_read(&path).map_err(io_error)?;
    let expected = source.output_bytes.ok_or_else(|| {
        Failure::new(
            "media.mux_input_invalid",
            "The input media task has no verified output size.",
        )
    })?;
    if input.len().map_err(io_error)? != expected {
        return Err(Failure::new(
            "media.mux_input_changed",
            "The completed media file changed on disk.",
        ));
    }
    let target = directory.join(format!("segment-{index:04}.bin"));
    let output = RealFs.create_new(&target).map_err(io_error)?;
    let mut offset = 0u64;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 64 * 1024];
    while offset < expected {
        if progress.cancel.load(Ordering::Acquire) {
            return Err(canceled());
        }
        if Instant::now() >= deadline {
            return Err(Failure::new(
                "media.helper_timeout",
                "Media input staging reached the reviewed time limit.",
            ));
        }
        let count = buffer.len().min((expected - offset) as usize);
        input
            .read_exact_at(offset, &mut buffer[..count])
            .map_err(io_error)?;
        digest.update(&buffer[..count]);
        output
            .write_all_at(offset, &buffer[..count])
            .map_err(io_error)?;
        offset += count as u64;
    }
    let digest = hex::encode(digest.finalize());
    if source.sha256.as_deref() != Some(&digest) {
        return Err(Failure::new(
            "media.mux_input_changed",
            "The completed media file failed SHA-256 verification.",
        ));
    }
    output.sync_all().map_err(io_error)?;
    let receipt = Receipt {
        bytes: offset,
        sha256: digest.clone(),
        etag: None,
    };
    if !transfer::verify_opened(
        output.as_ref(),
        &receipt,
        Some(progress.cancel.as_ref()),
        Some(deadline),
    )? {
        return Err(Failure::new(
            "media.mux_input_changed",
            "A private media input failed read-back SHA-256 verification.",
        ));
    }
    drop(output);
    RealFs.set_readonly(&target, true).map_err(io_error)?;
    Ok(receipt)
}

/// Run the pinned helper only after independent, verified input copies exist.
/// Output is received through a byte-capped pipe, rehashed and published with
/// an exclusive rename inside the configured destination root.
pub(crate) async fn execute(
    pin: Pin,
    inputs: Vec<Checkpoint>,
    container: String,
    directory: PathBuf,
    progress: Arc<Progress>,
    timeout: Duration,
) -> Result<()> {
    let started = Instant::now();
    let deadline = started + timeout;
    for (index, source) in inputs.iter().cloned().enumerate() {
        let (copy_dir, copy_progress) = (directory.clone(), progress.clone());
        let receipt = tokio::task::spawn_blocking(move || {
            copy_input(source, index, copy_dir, copy_progress, deadline)
        })
        .await
        .map_err(|_| Failure::new("media.filesystem", "The media staging worker stopped."))??;
        progress
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media state is unavailable."))?
            .receipts
            .insert(index, receipt);
        transfer::save(&directory, &progress)?;
    }
    let checkpoint = progress.snapshot()?;
    let root = SafeRoot::open(&checkpoint.root).map_err(io_error)?;
    let target = root
        .join_checked(Path::new(&checkpoint.filename))
        .map_err(io_error)?;
    root.check_existing(&target).map_err(io_error)?;
    if RealFs.metadata(&target).map_err(io_error)?.is_some() {
        return Err(Failure::new(
            "media.destination_exists",
            "The converted output already exists. Choose a new filename.",
        ));
    }
    let receipt = convert(&pin, &inputs, &container, &directory, &progress, deadline).await?;
    {
        let mut state = progress
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media state is unavailable."))?;
        // Commit the helper's flushed output receipt before publication. A crash
        // can then recover the exact bytes without executing the helper again.
        state.output_bytes = Some(receipt.bytes);
        state.sha256 = Some(receipt.sha256.clone());
        state.assembly_started = true;
    }
    transfer::save(&directory, &progress)?;
    let (publish_dir, publish_progress) = (directory.clone(), progress.clone());
    tokio::task::spawn_blocking(move || {
        publish(
            &publish_dir,
            &publish_progress,
            &receipt,
            Some(deadline),
            false,
        )
    })
    .await
    .map_err(|_| Failure::new("media.filesystem", "Media publication stopped."))??;
    complete(&directory, &progress)
}

#[derive(Clone)]
struct LocalInput {
    format: &'static str,
    path: PathBuf,
}

struct Conversion<'a> {
    pin: &'a Pin,
    directory: &'a Path,
    progress: &'a Arc<Progress>,
    deadline: Instant,
    ceiling: u64,
    spent: u64,
}

impl Conversion<'_> {
    async fn run(
        &mut self,
        inputs: &[LocalInput],
        container: &str,
        aac_framing: bool,
        path: &Path,
    ) -> Result<Receipt> {
        let remaining = self
            .deadline
            .checked_duration_since(Instant::now())
            .ok_or_else(|| {
                Failure::new(
                    "media.helper_timeout",
                    "Media conversion reached its reviewed time limit.",
                )
            })?;
        let available = self
            .ceiling
            .checked_sub(self.spent)
            .filter(|bytes| *bytes > 0)
            .ok_or_else(|| {
                Failure::new(
                    "media.output_limit",
                    "The reviewed total helper output ceiling was reached.",
                )
            })?;
        let output: Arc<dyn DataFile> = Arc::from(RealFs.create_new(path).map_err(io_error)?);
        let result = runner::run(
            self.pin,
            &arguments(inputs, container, aac_framing)?,
            self.directory,
            output.clone(),
            available,
            remaining,
            self.progress.cancel.clone(),
        )
        .await;
        drop(output);
        let written = RealFs
            .metadata(path)
            .map_err(io_error)?
            .map_or(0, |metadata| metadata.len);
        self.spent = self
            .spent
            .checked_add(written)
            .filter(|bytes| *bytes <= self.ceiling)
            .ok_or_else(|| {
                Failure::new(
                    "media.output_limit",
                    "The reviewed total helper output ceiling was reached.",
                )
            })?;
        self.progress.received.store(self.spent, Ordering::Release);
        result
    }
}

async fn convert(
    pin: &Pin,
    inputs: &[Checkpoint],
    container: &str,
    directory: &Path,
    progress: &Arc<Progress>,
    deadline: Instant,
) -> Result<Receipt> {
    let local = inputs
        .iter()
        .enumerate()
        .map(|(index, input)| {
            Ok(LocalInput {
                format: demuxer(&input.filename)?,
                path: directory.join(format!("segment-{index:04}.bin")),
            })
        })
        .collect::<Result<Vec<_>>>()?;
    let may_need_aac = local
        .last()
        .is_some_and(|input| matches!(input.format, "mpegts" | "aac"));
    let mut context = Conversion {
        pin,
        directory,
        progress,
        deadline,
        ceiling: progress.snapshot()?.max_bytes,
        spent: 0,
    };
    let output_path = directory.join("converted-output.part");
    match context.run(&local, container, false, &output_path).await {
        Ok(receipt) => return Ok(receipt),
        Err(error)
            if may_need_aac
                && matches!(
                    error.code.as_str(),
                    "media.helper_failed" | "media.helper_empty"
                ) =>
        {
            RealFs.remove_file(&output_path).map_err(io_error)?;
        }
        Err(error) => return Err(error),
    }
    if container == "mp4" {
        return context.run(&local, "mp4", true, &output_path).await;
    }
    // Matroska needs AAC AudioSpecificConfig before writing its track header.
    // A bounded fragmented-MP4 intermediate supplies it without re-encoding.
    let intermediate = directory.join("framing-intermediate.mp4");
    let receipt = context.run(&local, "mp4", true, &intermediate).await?;
    let file = RealFs.open_read(&intermediate).map_err(io_error)?;
    if !transfer::verify_opened(
        file.as_ref(),
        &receipt,
        Some(progress.cancel.as_ref()),
        Some(deadline),
    )? {
        return Err(Failure::new(
            "media.output_changed",
            "The AAC framing intermediate failed read-back verification.",
        ));
    }
    drop(file);
    context
        .run(
            &[LocalInput {
                format: "mov",
                path: intermediate,
            }],
            "mkv",
            false,
            &output_path,
        )
        .await
}

fn complete(directory: &Path, progress: &Progress) -> Result<()> {
    {
        let mut state = progress
            .checkpoint
            .lock()
            .map_err(|_| Failure::new("media.state", "The media state is unavailable."))?;
        state.state = "complete".into();
        state.error_code = None;
        state.error_message = None;
    }
    transfer::save(directory, progress)
}

fn publish(
    directory: &Path,
    progress: &Progress,
    receipt: &Receipt,
    deadline: Option<Instant>,
    recognize_existing: bool,
) -> Result<()> {
    let checkpoint = progress.snapshot()?;
    let root = SafeRoot::open(&checkpoint.root).map_err(io_error)?;
    let target = root
        .join_checked(Path::new(&checkpoint.filename))
        .map_err(io_error)?;
    root.check_existing(&target).map_err(io_error)?;
    if RealFs.metadata(&target).map_err(io_error)?.is_some() {
        if recognize_existing {
            let existing = RealFs.open_read(&target).map_err(io_error)?;
            if transfer::verify_opened(
                existing.as_ref(),
                receipt,
                Some(progress.cancel.as_ref()),
                deadline,
            )? {
                RealFs.sync_dir(root.path()).map_err(io_error)?;
                return Ok(());
            }
        }
        return Err(Failure::new(
            "media.destination_exists",
            "An existing converted output differs from this task. It was preserved.",
        ));
    }
    let input = RealFs
        .open_read(&directory.join("converted-output.part"))
        .map_err(io_error)?;
    if input.len().map_err(io_error)? != receipt.bytes {
        return Err(Failure::new(
            "media.output_changed",
            "The retained converter output has changed length.",
        ));
    }
    let temporary = root
        .join_checked(Path::new(&format!(".zeus-mux-{}.part", checkpoint.task_id)))
        .map_err(io_error)?;
    root.check_existing(&temporary).map_err(io_error)?;
    if RealFs.metadata(&temporary).map_err(io_error)?.is_some() {
        RealFs.remove_file(&temporary).map_err(io_error)?;
    }
    let destination = RealFs.create_new(&temporary).map_err(io_error)?;
    let result = (|| {
        let mut offset = 0u64;
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 64 * 1024];
        while offset < receipt.bytes {
            if progress.cancel.load(Ordering::Acquire) {
                return Err(canceled());
            }
            if deadline.is_some_and(|deadline| Instant::now() >= deadline) {
                return Err(Failure::new(
                    "media.helper_timeout",
                    "Media publication reached its reviewed time limit.",
                ));
            }
            let count = buffer.len().min((receipt.bytes - offset) as usize);
            input
                .read_exact_at(offset, &mut buffer[..count])
                .map_err(io_error)?;
            digest.update(&buffer[..count]);
            destination
                .write_all_at(offset, &buffer[..count])
                .map_err(io_error)?;
            offset += count as u64;
        }
        if hex::encode(digest.finalize()) != receipt.sha256 {
            return Err(Failure::new(
                "media.output_changed",
                "Retained converter output failed SHA-256 verification.",
            ));
        }
        destination.sync_all().map_err(io_error)?;
        if !transfer::verify_opened(
            destination.as_ref(),
            receipt,
            Some(progress.cancel.as_ref()),
            deadline,
        )? {
            return Err(Failure::new(
                "media.output_changed",
                "Converted output failed read-back SHA-256 verification.",
            ));
        }
        NativeMarker
            .apply(
                &temporary,
                &ProvenanceMarks {
                    origin_url: None,
                    referrer_url: None,
                    retrieved_at_unix_s: std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |n| n.as_secs() as i64),
                },
                zeus_types::OsMetadataMode::MarkOnly,
            )
            .map_err(io_error)?;
        root.check_existing(&target).map_err(io_error)?;
        RealFs
            .rename_noreplace(&temporary, &target)
            .map_err(io_error)?;
        RealFs.sync_dir(root.path()).map_err(io_error)?;
        Ok(())
    })();
    drop(destination);
    if result.is_err() {
        let _ = RealFs.remove_file(&temporary);
    }
    result
}

/// Recover the exact durable converter output after interruption. No source
/// request, helper configuration, helper execution or re-encoding is involved.
pub(crate) async fn recover(directory: PathBuf, progress: Arc<Progress>) -> Result<()> {
    let state = progress.snapshot()?;
    let receipt = Receipt {
        bytes: state.output_bytes.ok_or_else(|| {
            Failure::new(
                "media.recovery_incomplete",
                "The converter has no durable output receipt.",
            )
        })?,
        sha256: state.sha256.ok_or_else(|| {
            Failure::new(
                "media.recovery_incomplete",
                "The converter has no durable output digest.",
            )
        })?,
        etag: None,
    };
    let (publish_dir, publish_progress) = (directory.clone(), progress.clone());
    tokio::task::spawn_blocking(move || {
        publish(&publish_dir, &publish_progress, &receipt, None, true)
    })
    .await
    .map_err(|_| Failure::new("media.filesystem", "Converter output recovery stopped."))??;
    complete(&directory, &progress)
}
