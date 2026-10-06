//! Local socket transport: connect (implemented) and bind (hardening not implemented yet).
//!
//! Windows uses a named pipe, Unix a domain socket (ADR 0002). The listener MUST be created with:
//! * Windows: `FILE_FLAG_FIRST_PIPE_INSTANCE` (a squatted name makes the daemon refuse to start),
//!   `PIPE_REJECT_REMOTE_CLIENTS`, and a DACL granting access only to the current user (and LocalSystem);
//! * Unix: socket mode `0600` inside a directory created `0700`; then, after every accept, the peer's credentials
//!   (`stream.peer_creds()` of the `interprocess` crate: pid and uid on Unix) are compared to the daemon's own, and
//!   on Windows the pipe client's token user is compared to the daemon's.
//!
//! That hardening needs platform FFI for the security descriptor and token comparison; it belongs in isolated
//! submodules of this module that opt in with `#[allow(unsafe_code)]` and a safety comment. Until it is written,
//! [`bind`] returns `api.not_implemented` rather than create an unprotected listener.

use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::traits::tokio::Stream as _;

use crate::endpoint::Endpoint;
use crate::error::{ApiError, ApiResult, codes};

/// A connected local-socket stream.
pub type IpcStream = Stream;

/// Connect to the daemon at `endpoint`. Authentication happens afterwards, in the handshake; the connection by
/// itself proves nothing about who answers.
pub async fn connect(endpoint: &Endpoint) -> ApiResult<IpcStream> {
    let name = socket_name(endpoint)?;
    Stream::connect(name).await.map_err(|e| {
        ApiError::new(
            codes::UNAVAILABLE,
            format!("cannot reach the Zeus daemon at {endpoint}: {}", e.kind()),
        )
    })
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

/// Create the hardened listener. Not implemented in the skeleton (see the module documentation).
pub fn bind(_endpoint: &Endpoint) -> ApiResult<()> {
    Err(ApiError::not_implemented(
        "zeus_api::transport::bind (first-instance, user-only DACL or mode 0600, peer check)",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn connecting_to_nothing_is_a_clean_unavailable_error() {
        let endpoint = if cfg!(windows) {
            Endpoint::NamedPipe(crate::endpoint::pipe_name_for(
                "zeus-test-nobody-listens-here",
            ))
        } else {
            let dir = tempfile::tempdir().unwrap();
            Endpoint::UnixSocket(dir.path().join("none.sock"))
        };
        let error = connect(&endpoint).await.unwrap_err();
        assert_eq!(error.envelope().code.as_str(), codes::UNAVAILABLE);
        assert!(error.envelope().retryable);
    }

    #[test]
    fn bind_is_explicitly_not_implemented() {
        let endpoint = Endpoint::NamedPipe(crate::endpoint::pipe_name_for("x"));
        assert_eq!(
            bind(&endpoint).unwrap_err().envelope().code.as_str(),
            codes::NOT_IMPLEMENTED
        );
    }
}
