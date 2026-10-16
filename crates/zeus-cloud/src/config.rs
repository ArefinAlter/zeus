use crate::{
    BaiduNetdisk, CloudError, CloudProvider, CloudResult, GoogleDrive, ProviderKind, Rclone,
    RcloneConfig, SecretString, YandexDisk,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Human-configured, nonsecret provider settings. Tokens live in a separate credential store.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "provider", rename_all = "snake_case", deny_unknown_fields)]
pub enum ProviderConfig {
    /// Existing Drive folder ID; `root` is allowed. OAuth bearer token supplied separately.
    GoogleDrive {
        /// Parent folder ID.
        parent_id: String,
    },
    /// Existing Disk folder, e.g. `disk:/Zeus`. OAuth token supplied separately.
    YandexDisk {
        /// Existing root folder.
        root_path: String,
    },
    /// Explicit external executable, config file and remote. Never auto-discovered.
    Rclone {
        /// Adapter settings.
        config: RcloneConfig,
    },
    /// Native multipart upload into the authorized application's directory.
    BaiduNetdisk {
        /// Existing root under `/apps/<appname>`.
        root_path: String,
    },
}

impl ProviderConfig {
    /// Validate nonsecret settings without fetching credentials or contacting a provider.
    pub fn validate(&self) -> CloudResult<()> {
        match self {
            Self::GoogleDrive { parent_id } => GoogleDrive::validate_parent(parent_id),
            Self::YandexDisk { root_path } => YandexDisk::validate_root(root_path),
            Self::Rclone { config } => config.validate(),
            Self::BaiduNetdisk { root_path } => BaiduNetdisk::validate_root(root_path),
        }
    }
    /// Provider family without opening credentials or network.
    pub fn kind(&self) -> ProviderKind {
        match self {
            Self::GoogleDrive { .. } => ProviderKind::GoogleDrive,
            Self::YandexDisk { .. } => ProviderKind::YandexDisk,
            Self::Rclone { .. } => ProviderKind::Rclone,
            Self::BaiduNetdisk { .. } => ProviderKind::BaiduNetdisk,
        }
    }

    /// Construct a configured implementation. Rclone discovers the configured remote's actual
    /// backend capabilities using bounded output. No cloud mutation occurs here.
    pub async fn connect(
        &self,
        credential: Option<SecretString>,
        cancel: &CancellationToken,
    ) -> CloudResult<Arc<dyn CloudProvider>> {
        match self {
            Self::GoogleDrive { parent_id } => Ok(Arc::new(GoogleDrive::new(
                credential.ok_or(CloudError::CredentialUnavailable)?,
                parent_id.clone(),
            )?)),
            Self::YandexDisk { root_path } => Ok(Arc::new(YandexDisk::new(
                credential.ok_or(CloudError::CredentialUnavailable)?,
                root_path.clone(),
            )?)),
            Self::Rclone { config } => Ok(Arc::new(Rclone::connect(config.clone(), cancel).await?)),
            Self::BaiduNetdisk { root_path } => Ok(Arc::new(BaiduNetdisk::new(
                credential.ok_or(CloudError::CredentialUnavailable)?,
                root_path.clone(),
            )?)),
        }
    }
}
