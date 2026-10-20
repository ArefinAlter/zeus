use crate::{CloudError, CloudResult, SecretString};
use reqwest::{Client, RequestBuilder, Response};
use std::time::Duration;
use tokio_util::sync::CancellationToken;
use url::Url;

pub(crate) const RESPONSE_CAP: usize = 64 * 1024;

pub(crate) struct ProviderHttp {
    pub(crate) client: Client,
    pub(crate) api: Url,
    credential: SecretString,
    oauth: bool,
}

impl ProviderHttp {
    pub(crate) fn new(api: &str, credential: SecretString, oauth: bool) -> CloudResult<Self> {
        if credential.expose().is_empty()
            || credential.expose().len() > 16 * 1024
            || credential.expose().bytes().any(|b| b.is_ascii_control())
        {
            return Err(CloudError::InvalidRequest);
        }
        let api = Url::parse(api).map_err(|_| CloudError::InvalidRequest)?;
        let client = Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .no_proxy()
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(300))
            .build()
            .map_err(|_| CloudError::Transport)?;
        Ok(Self {
            client,
            api,
            credential,
            oauth,
        })
    }

    pub(crate) fn url(&self, path: &str) -> CloudResult<Url> {
        self.api.join(path).map_err(|_| CloudError::InvalidRequest)
    }

    pub(crate) fn authorized(
        &self,
        method: reqwest::Method,
        url: Url,
    ) -> CloudResult<RequestBuilder> {
        if url.origin() != self.api.origin() {
            return Err(CloudError::UntrustedLocation);
        }
        let scheme = if self.oauth { "OAuth" } else { "Bearer" };
        let mut value =
            http::HeaderValue::from_str(&format!("{scheme} {}", self.credential.expose()))
                .map_err(|_| CloudError::CredentialUnavailable)?;
        value.set_sensitive(true);
        Ok(self
            .client
            .request(method, url)
            .header(http::header::AUTHORIZATION, value))
    }

    pub(crate) fn same_origin_location(&self, raw: &str, prefix: &str) -> CloudResult<Url> {
        let url = Url::parse(raw).map_err(|_| CloudError::UntrustedLocation)?;
        if url.origin() != self.api.origin()
            || !url.path().starts_with(prefix)
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(CloudError::UntrustedLocation);
        }
        Ok(url)
    }

    pub(crate) fn yandex_upload_location(&self, raw: &str) -> CloudResult<Url> {
        let url = Url::parse(raw).map_err(|_| CloudError::UntrustedLocation)?;
        let official = url.scheme() == "https"
            && url.port_or_known_default() == Some(443)
            && url.host_str().is_some_and(|h| {
                h.ends_with(".disk.yandex.net") && h.len() > ".disk.yandex.net".len()
            })
            && url.path().starts_with("/upload-target/");
        #[cfg(test)]
        let official = official
            || (url.origin() == self.api.origin() && url.path().starts_with("/upload-target/"));
        if !official
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(CloudError::UntrustedLocation);
        }
        Ok(url)
    }
}

pub(crate) async fn send(
    request: RequestBuilder,
    cancel: &CancellationToken,
) -> CloudResult<Response> {
    tokio::select! { biased;
        _ = cancel.cancelled() => Err(CloudError::Cancelled),
        result = request.send() => result.map_err(|_| CloudError::Transport),
    }
}

pub(crate) async fn json(
    mut response: Response,
    cancel: &CancellationToken,
) -> CloudResult<serde_json::Value> {
    let status = response.status();
    if !status.is_success() {
        return Err(CloudError::Http(status.as_u16()));
    }
    if response
        .content_length()
        .is_some_and(|n| n > RESPONSE_CAP as u64)
    {
        return Err(CloudError::ResponseTooLarge);
    }
    let mut result = Vec::new();
    loop {
        let chunk = tokio::select! { biased;
            _ = cancel.cancelled() => return Err(CloudError::Cancelled),
            result = response.chunk() => result.map_err(|_| CloudError::Transport)?,
        };
        let Some(chunk) = chunk else { break };
        if result.len().saturating_add(chunk.len()) > RESPONSE_CAP {
            return Err(CloudError::ResponseTooLarge);
        }
        result.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&result).map_err(|_| CloudError::Protocol)
}

pub(crate) fn string(value: &serde_json::Value, key: &str) -> CloudResult<String> {
    value
        .get(key)
        .and_then(|v| v.as_str())
        .filter(|s| !s.is_empty() && s.len() < 8192)
        .map(str::to_owned)
        .ok_or(CloudError::Protocol)
}

pub(crate) fn safe_id(value: &str) -> CloudResult<()> {
    if value.is_empty()
        || value.len() > 256
        || !value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
    {
        return Err(CloudError::Protocol);
    }
    Ok(())
}

pub(crate) fn size(value: &serde_json::Value, key: &str) -> CloudResult<u64> {
    value
        .get(key)
        .and_then(|v| {
            v.as_u64()
                .or_else(|| v.as_str().and_then(|s| s.parse().ok()))
        })
        .ok_or(CloudError::Protocol)
}
