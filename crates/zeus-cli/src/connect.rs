//! Connecting to the daemon: credential and endpoint resolution, then the authenticated handshake.
//!
//! Credentials live in `<state>/credentials/<name>.token` and the IPC secret in `<state>/ipc.secret`, both created
//! owner-only by `zeusd init` / `zeus daemon install`. The CLI uses the `cli` credential (kind `admin`, role `cli`),
//! `zeus mcp` uses `agent` (kind `agent`, role `mcp`). A missing file means "not installed", reported as
//! `api.unavailable` with the command that fixes it.

use zeus_api::auth::{ClientCredential, IpcSecret};
use zeus_api::client::ClientConfig;
use zeus_api::endpoint::ZeusPaths;
use zeus_api::error::codes;
use zeus_api::methods::daemon::ClientInfo;
use zeus_api::{ApiError, ApiResult, ClientRole, IpcClient};

/// Which stored credential to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Identity {
    /// The human's `zeus` command line (`cli.token`).
    Cli,
    /// The MCP server (`agent.token`).
    Agent,
    /// The standalone A2A bridge (`a2a.token`).
    A2a,
}

impl Identity {
    /// The credential file name stem.
    pub const fn file_stem(self) -> &'static str {
        match self {
            Self::Cli => "cli",
            Self::Agent => "agent",
            Self::A2a => "a2a",
        }
    }

    /// The role presented in `daemon.hello`.
    pub const fn role(self) -> ClientRole {
        match self {
            Self::Cli => ClientRole::Cli,
            Self::Agent => ClientRole::Mcp,
            Self::A2a => ClientRole::A2a,
        }
    }
}

fn not_installed(what: &str) -> ApiError {
    ApiError::new(
        codes::UNAVAILABLE,
        format!("{what} not found; run `zeus daemon install` first"),
    )
}

/// Read the IPC secret.
pub fn load_secret(paths: &ZeusPaths) -> ApiResult<IpcSecret> {
    let text = std::fs::read_to_string(paths.ipc_secret_file())
        .map_err(|_| not_installed("the IPC secret"))?;
    IpcSecret::from_file_text(&text)
}

/// Read a credential.
pub fn load_credential(paths: &ZeusPaths, identity: Identity) -> ApiResult<ClientCredential> {
    let text = std::fs::read_to_string(paths.credential_file(identity.file_stem()))
        .map_err(|_| not_installed("the client credential"))?;
    ClientCredential::from_file_text(&text)
}

/// Connect and authenticate as `identity`.
pub async fn connect(paths: &ZeusPaths, identity: Identity) -> ApiResult<IpcClient> {
    let secret = load_secret(paths)?;
    let credential = load_credential(paths, identity)?;
    let info = ClientInfo {
        name: "zeus-cli".into(),
        version: env!("CARGO_PKG_VERSION").into(),
        role: identity.role(),
    };
    IpcClient::connect(
        &paths.endpoint(),
        credential,
        &secret,
        ClientConfig::new(info),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_install_is_reported_with_the_fix() {
        let dir = tempfile::tempdir().unwrap();
        let paths = ZeusPaths {
            state_dir: dir.path().join("state"),
            config_dir: dir.path().join("config"),
            runtime_dir: dir.path().join("run"),
            user_identity: "t".into(),
            profile: None,
        };
        let error = connect(&paths, Identity::Cli).await.err().unwrap();
        assert_eq!(error.envelope().code.as_str(), codes::UNAVAILABLE);
        assert!(error.envelope().message.contains("zeus daemon install"));
    }

    #[test]
    fn identities_map_to_roles_and_files() {
        assert_eq!(Identity::Cli.role(), ClientRole::Cli);
        assert_eq!(Identity::Agent.role(), ClientRole::Mcp);
        assert_eq!(
            (Identity::Cli.file_stem(), Identity::Agent.file_stem()),
            ("cli", "agent")
        );
    }
}
