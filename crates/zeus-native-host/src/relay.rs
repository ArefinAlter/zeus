//! Bounded, lossless native-messaging relay. Frame contents are never logged.

use serde_json::{Value, json};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeus_api::auth::{ClientCredential, IpcSecret};
use zeus_api::client::ClientConfig;
use zeus_api::endpoint::ZeusPaths;
use zeus_api::methods::daemon::{AttachInfo, ClientInfo};
use zeus_api::{Client, ClientRole, IpcClient};

use crate::argv::Launch;
use crate::error::HostError;
use crate::frame::{MAX_FROM_EXTENSION, MAX_TO_EXTENSION, parse_payload};

fn daemon_error(_: zeus_api::ApiError) -> HostError {
    // Authentication and daemon errors must never print frame bodies or credentials.
    HostError::DaemonUnavailable
}

/// Run until either peer disconnects. Parent verification comes from OS process metadata and
/// installed browser registration; argv alone is never proof of a browser launch.
pub async fn run(launch: &Launch) -> Result<(), HostError> {
    let paths = ZeusPaths::discover().map_err(daemon_error)?;
    let secret = IpcSecret::from_file_text(&std::fs::read_to_string(paths.ipc_secret_file())?)
        .map_err(daemon_error)?;
    let credential =
        ClientCredential::from_file_text(&std::fs::read_to_string(paths.credential_file("host"))?)
            .map_err(daemon_error)?;
    let mut config = ClientConfig::new(ClientInfo {
        name: "zeus-native-host".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        role: ClientRole::NativeHost,
    });
    config.attach = Some(AttachInfo {
        browser: launch.browser,
        attested_extension: launch.extension_id.clone(),
        host_version: env!("CARGO_PKG_VERSION").into(),
        parent_verified: crate::parent::verify(launch.browser),
    });
    let client = IpcClient::connect(&paths.endpoint(), credential, &secret, config)
        .await
        .map_err(daemon_error)?;
    relay(&client, tokio::io::stdin(), tokio::io::stdout()).await
}

async fn read_native<R: AsyncRead + Unpin>(reader: &mut R) -> Result<Option<Value>, HostError> {
    let mut prefix = [0u8; 4];
    if reader.read(&mut prefix[..1]).await? == 0 {
        return Ok(None);
    }
    reader
        .read_exact(&mut prefix[1..])
        .await
        .map_err(truncated)?;
    let length = u32::from_ne_bytes(prefix) as usize;
    if length == 0 {
        return Err(HostError::ZeroLength);
    }
    if length > MAX_FROM_EXTENSION {
        return Err(HostError::FrameTooLarge {
            length: length as u64,
            limit: MAX_FROM_EXTENSION,
        });
    }
    let mut bytes = vec![0; length];
    reader.read_exact(&mut bytes).await.map_err(truncated)?;
    parse_payload(&bytes).map(Some)
}

fn truncated(error: std::io::Error) -> HostError {
    if error.kind() == std::io::ErrorKind::UnexpectedEof {
        HostError::Truncated
    } else {
        HostError::Io(error)
    }
}

async fn write_native<W: AsyncWrite + Unpin>(
    writer: &mut W,
    bytes: &[u8],
) -> Result<(), HostError> {
    if bytes.is_empty() {
        return Err(HostError::ZeroLength);
    }
    if bytes.len() > MAX_TO_EXTENSION {
        return Err(HostError::FrameTooLarge {
            length: bytes.len() as u64,
            limit: MAX_TO_EXTENSION,
        });
    }
    writer
        .write_all(&(bytes.len() as u32).to_ne_bytes())
        .await?;
    writer.write_all(bytes).await?;
    writer.flush().await?;
    Ok(())
}

/// Relay over injectable streams. One reader task keeps partial reads intact when a daemon message
/// arrives; a bounded channel applies backpressure instead of dropping native-messaging frames.
pub async fn relay<R, W>(client: &IpcClient, mut input: R, mut output: W) -> Result<(), HostError>
where
    R: AsyncRead + Unpin + Send + 'static,
    W: AsyncWrite + Unpin,
{
    let mut notifications = client.notifications();
    let (sender, mut incoming) = tokio::sync::mpsc::channel(8);
    let reader = tokio::spawn(async move {
        let mut invalid = 0;
        loop {
            let result = read_native(&mut input).await;
            if matches!(result, Err(HostError::InvalidJson)) {
                invalid += 1;
                if invalid < 3 {
                    continue;
                }
            } else {
                invalid = 0;
            }
            let end = !matches!(result, Ok(Some(_)));
            if sender.send(result).await.is_err() || end {
                break;
            }
        }
    });
    // Aborting the async reader is essential on daemon EOF (stdin may remain open indefinitely).
    let result = async {
        let mut host_id = 0u64;
        loop {
            tokio::select! {
                frame = incoming.recv() => match frame {
                    Some(Ok(Some(frame))) => client.notify("nm.frame", json!({"frame": frame})).await.map_err(daemon_error)?,
                    Some(Err(error)) => return Err(error),
                    Some(Ok(None)) | None => return Ok(()),
                },
                _ = client.closed() => return Ok(()),
                notification = notifications.recv() => {
                    let note = notification.map_err(|_| HostError::DaemonUnavailable)?;
                    if note.method == "daemon.stopping" {
                        host_id += 1;
                        let bye = json!({"v":1,"id":format!("h{host_id}"),"type":"bye","body":{"reason":"daemon_gone"}});
                        write_native(&mut output, &serde_json::to_vec(&bye).map_err(|_| HostError::InvalidJson)?).await?;
                    } else if note.method == "nm.frame" {
                        let frame = note.params.as_ref().and_then(|p| p.get("frame")).filter(|v| v.is_object())
                            .ok_or(HostError::InvalidJson)?;
                        let bytes = serde_json::to_vec(frame).map_err(|_| HostError::InvalidJson)?;
                        if bytes.len() > MAX_TO_EXTENSION {
                            host_id += 1;
                            let error = json!({"v":1,"id":format!("h{host_id}"),"re":frame.get("id"),"type":"error","body":{"error":{
                                "code":"nm.frame_too_large","class":"invalid_request","message":"A daemon frame exceeded 1000000 bytes and was dropped.","retryable":false,"details":{"limit_bytes":MAX_TO_EXTENSION}
                            }}});
                            client.notify("nm.frame", json!({"source":"host","frame":error})).await.map_err(daemon_error)?;
                        } else { write_native(&mut output, &bytes).await?; }
                    }
                }
            }
        }
    }.await;
    reader.abort();
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn async_framing_roundtrips_and_reports_truncation() {
        let (mut a, mut b) = tokio::io::duplex(128);
        write_native(&mut a, br#"{"v":1,"id":"e1","type":"ping","body":{}}"#)
            .await
            .unwrap();
        assert_eq!(read_native(&mut b).await.unwrap().unwrap()["type"], "ping");
        a.write_all(&[3, 0]).await.unwrap();
        drop(a);
        assert!(matches!(
            read_native(&mut b).await,
            Err(HostError::Truncated)
        ));
    }
    #[tokio::test]
    async fn oversized_prefix_never_allocates_or_waits_for_payload() {
        let (mut a, mut b) = tokio::io::duplex(8);
        a.write_all(&u32::MAX.to_ne_bytes()).await.unwrap();
        assert!(matches!(
            read_native(&mut b).await,
            Err(HostError::FrameTooLarge { .. })
        ));
    }
}
