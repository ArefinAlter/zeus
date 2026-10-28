//! Dedicated loopback MCP token creation in this installation's private state directory.
use serde::Serialize;
use std::{
    io::Write,
    path::{Path, PathBuf},
};
use zeus_api::{ApiError, ApiResult};
/// Create a new owner-only bearer file without printing its secret or overwriting anything.
pub fn initialize_token(state: &Path) -> ApiResult<PathBuf> {
    std::fs::create_dir_all(state)
        .map_err(|_| ApiError::internal("MCP state directory unavailable"))?;
    zeus_api::transport::secure_private_directory(state)
        .map_err(|_| ApiError::internal("MCP state directory cannot be made private"))?;
    let path = state.join("mcp-http-token.json");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(&path).map_err(|_| {
        ApiError::invalid_params("MCP token file already exists or cannot be created")
    })?;
    // No secret is written before the platform helper protects the newly created empty file.
    zeus_api::transport::secure_private_file(&path)
        .map_err(|_| ApiError::internal("MCP token file cannot be made private"))?;
    let mut secret = zeus_api::auth::Token::generate().to_text();
    secret.replace_range(..4, "zmcp1_");
    #[derive(Serialize)]
    struct Dedicated<'a> {
        token: &'a str,
    }
    serde_json::to_writer(
        &mut file,
        &Dedicated {
            token: secret.as_str(),
        },
    )
    .map_err(|_| ApiError::internal("MCP token file write failed"))?;
    file.write_all(b"\n")
        .map_err(|_| ApiError::internal("MCP token file write failed"))?;
    file.sync_all()
        .map_err(|_| ApiError::internal("MCP token file could not be synchronized"))?;
    Ok(path)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tokens_are_private_dedicated_and_never_overwritten() {
        let temp = tempfile::tempdir().unwrap();
        let state = temp.path().join("state");
        let path = initialize_token(&state).unwrap();
        let before = std::fs::read(&path).unwrap();
        assert!(zeus_mcp::http_bridge::token_file_hash(&path).is_ok());
        assert!(initialize_token(&state).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let ordinary = zeus_api::auth::Token::generate().to_text();
        std::fs::write(
            &path,
            serde_json::to_vec(&serde_json::json!({"token":ordinary.as_str()})).unwrap(),
        )
        .unwrap();
        assert!(zeus_mcp::http_bridge::token_file_hash(&path).is_err());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o077,
                0
            );
        }
    }
}
