//! Owner-only local IPC with OS peer checks before authentication.

mod platform;

pub use platform::{discard_terminal_input, secure_private_directory, secure_private_file};

use interprocess::local_socket::traits::StreamCommon as _;

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Listener as _;
use interprocess::local_socket::traits::tokio::Stream as _;
use interprocess::local_socket::{ListenerOptions, tokio::Listener};

use crate::endpoint::Endpoint;
use crate::error::{ApiError, ApiResult, codes};

/// A connected local-socket stream.
pub type IpcStream = Stream;

/// Process evidence obtained from the local socket and OS, never request parameters.
#[derive(Debug, Clone, Default)]
pub struct PeerProcess {
    /// OS supplied process identifier, when available.
    pub pid: Option<u32>,
    /// Executable reported by the OS.
    pub executable: Option<std::path::PathBuf>,
    /// Controlling terminal on Linux, non-service interactive session on Windows.
    pub interactive: bool,
}

/// Inspect an already authenticated same-user transport peer; unavailable evidence fails closed.
pub fn peer_process(stream: &IpcStream) -> PeerProcess {
    let pid = stream.peer_creds().ok().and_then(|p| p.pid());
    let details = pid.and_then(|pid| platform::process_details(pid).ok());
    PeerProcess {
        pid,
        interactive: details.as_ref().is_some_and(|(_, tty)| *tty),
        executable: details.map(|(path, _)| path),
    }
}

/// Connect to the daemon at `endpoint`. Authentication happens afterwards, in the handshake; the connection by
/// itself proves nothing about who answers.
pub async fn connect(endpoint: &Endpoint) -> ApiResult<IpcStream> {
    let name = socket_name(endpoint)?;
    let stream = Stream::connect(name).await.map_err(|e| {
        ApiError::new(
            codes::UNAVAILABLE,
            format!("cannot reach the Zeus daemon at {endpoint}: {}", e.kind()),
        )
    })?;
    verify_peer(&stream)?;
    Ok(stream)
}

#[cfg(windows)]
fn socket_name(endpoint: &Endpoint) -> ApiResult<interprocess::local_socket::Name<'static>> {
    use interprocess::local_socket::{GenericNamespaced, ToNsName};
    match endpoint {
        Endpoint::NamedPipe(full) => full
            .strip_prefix(r"\\.\pipe\")
            .unwrap_or(full)
            .to_string()
            .to_ns_name::<GenericNamespaced>()
            .map_err(|e| ApiError::internal(format!("invalid pipe name: {e}"))),
        Endpoint::UnixSocket(_) => Err(ApiError::internal("unix sockets are not used on Windows")),
    }
}

#[cfg(not(windows))]
fn socket_name(endpoint: &Endpoint) -> ApiResult<interprocess::local_socket::Name<'static>> {
    use interprocess::local_socket::{GenericFilePath, ToFsName};
    match endpoint {
        Endpoint::UnixSocket(path) => path
            .clone()
            .to_fs_name::<GenericFilePath>()
            .map_err(|e| ApiError::internal(format!("invalid socket path: {e}"))),
        Endpoint::NamedPipe(_) => Err(ApiError::internal("named pipes are only used on Windows")),
    }
}

/// OS-derived current user identifier; never trusts USER/USERNAME environment variables.
pub fn user_identity() -> ApiResult<String> {
    platform::user_identity()
        .map_err(|_| ApiError::new(codes::UNAVAILABLE, "cannot read OS user identity"))
}

/// Listener whose accepted streams have passed a same-user check.
pub struct IpcListener(Listener);

impl IpcListener {
    /// Accept and authenticate the operating-system peer identity.
    pub async fn accept(&self) -> ApiResult<IpcStream> {
        let stream = self
            .0
            .accept()
            .await
            .map_err(|_| ApiError::new(codes::UNAVAILABLE, "IPC accept failed"))?;
        verify_peer(&stream)?;
        Ok(stream)
    }
}

/// Verify the identity supplied by the OS rather than a caller's claimed pid or username.
fn verify_peer(stream: &IpcStream) -> ApiResult<()> {
    let denied = || {
        ApiError::new(
            codes::PERMISSION_DENIED,
            "IPC peer is not the current OS user",
        )
    };
    let peer = stream.peer_creds().map_err(|_| denied())?;
    #[cfg(unix)]
    let matches = peer.euid().map(|uid| uid.to_string()) == Some(user_identity()?);
    #[cfg(windows)]
    let matches = peer
        .pid()
        .and_then(|pid| platform::process_identity(pid).ok())
        == Some(user_identity()?);
    if matches { Ok(()) } else { Err(denied()) }
}

/// Bind with a user-only DACL or private Unix directory and socket mode.
/// Windows interprocess listeners use FIRST_PIPE_INSTANCE and reject remote clients.
pub fn bind(endpoint: &Endpoint) -> ApiResult<IpcListener> {
    let options = ListenerOptions::new().name(socket_name(endpoint)?);
    #[cfg(windows)]
    let options = {
        use interprocess::os::windows::{
            local_socket::ListenerOptionsExt, security_descriptor::SecurityDescriptor,
        };
        let sddl = widestring::U16CString::from_str(format!(
            "D:P(A;;GA;;;SY)(A;;GA;;;{})",
            user_identity()?
        ))
        .map_err(|_| ApiError::internal("invalid OS identity"))?;
        let descriptor = SecurityDescriptor::deserialize(&sddl)
            .map_err(|_| ApiError::internal("cannot create owner-only pipe security descriptor"))?;
        options.security_descriptor(descriptor)
    };
    #[cfg(unix)]
    let options = {
        use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
        let Endpoint::UnixSocket(path) = endpoint else {
            return Err(ApiError::invalid_params("expected Unix socket"));
        };
        let dir = path
            .parent()
            .ok_or_else(|| ApiError::invalid_params("socket has no parent"))?;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
            .map_err(|_| {
                ApiError::new(codes::UNAVAILABLE, "cannot create private socket directory")
            })?;
        let metadata = std::fs::symlink_metadata(dir)
            .map_err(|_| ApiError::new(codes::UNAVAILABLE, "cannot inspect socket directory"))?;
        if !metadata.is_dir()
            || metadata.file_type().is_symlink()
            || metadata.uid().to_string() != user_identity()?
            || metadata.permissions().mode() & 0o077 != 0
        {
            return Err(ApiError::new(
                codes::PERMISSION_DENIED,
                "socket directory must be owned by this user with mode 0700",
            ));
        }
        // macOS does not support fchmod-before-bind; its owner-only parent prevents races.
        #[cfg(not(target_os = "macos"))]
        {
            use interprocess::os::unix::local_socket::ListenerOptionsExt;
            options.mode(0o600)
        }
        #[cfg(target_os = "macos")]
        {
            options
        }
    };
    let listener = options.create_tokio().map_err(|_| {
        ApiError::new(
            codes::UNAVAILABLE,
            "cannot bind IPC endpoint; another daemon may already own it",
        )
    })?;
    #[cfg(target_os = "macos")]
    if let Endpoint::UnixSocket(path) = endpoint {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
            .map_err(|_| ApiError::new(codes::UNAVAILABLE, "cannot protect socket"))?;
    }
    Ok(IpcListener(listener))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn owner_can_connect_and_second_listener_cannot_replace_first() {
        let dir = tempfile::tempdir().unwrap();
        #[cfg(unix)]
        let endpoint = Endpoint::UnixSocket(dir.path().join("private/daemon.sock"));
        #[cfg(windows)]
        let endpoint = Endpoint::NamedPipe(crate::endpoint::pipe_name_for(
            &dir.path().display().to_string(),
        ));
        let listener = bind(&endpoint).unwrap();
        assert!(bind(&endpoint).is_err());
        let client = connect(&endpoint);
        let server = listener.accept();
        let (client, server) = tokio::join!(client, server);
        assert!(client.is_ok());
        assert!(server.is_ok());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn refuses_shared_socket_directory() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        std::fs::set_permissions(dir.path(), std::fs::Permissions::from_mode(0o777)).unwrap();
        let endpoint = Endpoint::UnixSocket(dir.path().join("daemon.sock"));
        assert!(bind(&endpoint).is_err());
    }
}
