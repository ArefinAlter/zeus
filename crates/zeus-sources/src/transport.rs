use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use futures::{StreamExt, stream};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use url::Url;
use zeus_http::{HttpRequest, HttpResponse, HttpTransport, TransportError, TransportFailure};

use crate::{
    ObjectMetadata, SourceAccountConfig, SourceAuthority, SourceError, SourceResult, SourceSecret,
    SourceTarget,
};

enum Provider {
    #[cfg(feature = "s3")]
    S3 {
        region: String,
        credentials: crate::s3::S3Credentials,
        version: Option<String>,
    },
    #[cfg(feature = "hugging-face")]
    Hub {
        commit: String,
        token: Option<SourceSecret>,
    },
}

/// Private immutable provider selection. Debug never reveals tokens or addresses.
pub struct BoundSource {
    url: Url,
    provider: Provider,
    metadata: Option<ObjectMetadata>,
}
impl std::fmt::Debug for BoundSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("BoundSource([PRIVATE])")
    }
}
impl BoundSource {
    /// Prepare only the native provider endpoint allowed by an explicit account.
    /// The token comes from a credential port selected by the embedding.
    pub fn prepare(
        config: &SourceAccountConfig,
        target: &SourceTarget,
        credential: Option<&SourceSecret>,
    ) -> SourceResult<Self> {
        #[allow(unreachable_patterns)]
        match (config, target) {
            #[cfg(feature = "s3")]
            (SourceAccountConfig::S3 { region, .. }, SourceTarget::S3 { version_id, .. }) => {
                Ok(Self {
                    url: crate::s3::object_url(config, target)?,
                    provider: Provider::S3 {
                        region: region.clone(),
                        credentials: crate::s3::S3Credentials::from_secret(
                            credential.ok_or(SourceError::Credential)?,
                        )?,
                        version: version_id.clone(),
                    },
                    metadata: None,
                })
            }
            #[cfg(feature = "hugging-face")]
            (SourceAccountConfig::HuggingFace { .. }, SourceTarget::HuggingFace { commit, .. }) => {
                if credential.is_some_and(|value| !crate::model::header_text(value.expose(), 8192))
                {
                    return Err(SourceError::Credential);
                }
                Ok(Self {
                    url: crate::hugging_face::object_url(config, target)?,
                    provider: Provider::Hub {
                        commit: commit.to_ascii_lowercase(),
                        token: credential.cloned(),
                    },
                    metadata: None,
                })
            }
            _ => Err(SourceError::Unsupported),
        }
    }

    /// Bind exact reviewed metadata before the object is admitted to an engine job.
    pub fn pin(mut self, metadata: ObjectMetadata) -> SourceResult<Self> {
        if metadata.bytes > 1 << 40
            || metadata.strong_etag.as_ref().is_some_and(|value| {
                !crate::model::header_text(value, 512) || value.starts_with("W/")
            })
        {
            return Err(SourceError::Invalid);
        }
        if metadata
            .sha256
            .as_ref()
            .is_some_and(|value| value.len() != 64 || !value.bytes().all(|b| b.is_ascii_hexdigit()))
        {
            return Err(SourceError::Invalid);
        }
        #[allow(unreachable_patterns)]
        match &self.provider {
            #[cfg(feature = "s3")]
            Provider::S3 { version, .. } if version.is_none() && metadata.strong_etag.is_none() => {
                return Err(SourceError::Protocol);
            }
            #[cfg(feature = "hugging-face")]
            Provider::Hub { commit, .. } if *commit != metadata.immutable_revision => {
                return Err(SourceError::Changed);
            }
            _ => {}
        }
        self.metadata = Some(metadata);
        Ok(self)
    }
}

/// Maps one opaque engine address to a private immutable source. It internally
/// follows provider redirects, strips Hub credentials at the CDN boundary, and
/// returns the opaque address to the engine. Signed CDN URLs never enter engine
/// job state, events or summaries. This is a virtual-object adapter above the
/// ordinary one-request HttpTransport, not a global redirecting HTTP client.
pub struct SourceTransport {
    source: Arc<BoundSource>,
    virtual_url: Url,
    inner: Arc<dyn HttpTransport>,
    authority: Arc<dyn SourceAuthority>,
}
impl SourceTransport {
    /// Bind a trusted durable authority port to one exact private virtual address.
    pub fn new(
        source: BoundSource,
        virtual_url: Url,
        inner: Arc<dyn HttpTransport>,
        authority: Arc<dyn SourceAuthority>,
    ) -> SourceResult<Self> {
        if virtual_url.scheme() != "https"
            || virtual_url.host_str() != Some("sources.zeus.invalid")
            || !virtual_url.username().is_empty()
            || virtual_url.password().is_some()
            || virtual_url.fragment().is_some()
        {
            return Err(SourceError::Invalid);
        }
        Ok(Self {
            source: Arc::new(source),
            virtual_url,
            inner,
            authority,
        })
    }

    /// HEAD metadata review under the same authority/redirect boundaries used by GET.
    /// No automatic download or job creation occurs.
    pub async fn inspect(&self) -> SourceResult<ObjectMetadata> {
        let request = HttpRequest {
            method: Method::HEAD,
            url: self.virtual_url.clone(),
            headers: HeaderMap::new(),
            head_timeout: Duration::from_secs(30),
            idle_timeout: Duration::from_secs(30),
        };
        let (response, hub_metadata) = self.request(request).await?;
        #[cfg(not(feature = "hugging-face"))]
        let _ = hub_metadata;
        if response.status != StatusCode::OK {
            return Err(status(response.status));
        }
        #[allow(unreachable_patterns)]
        match &self.source.provider {
            #[cfg(feature = "s3")]
            Provider::S3 { version, .. } => {
                let text = |name: &str| {
                    response
                        .headers
                        .get(name)
                        .and_then(|value| value.to_str().ok())
                };
                let bytes = text("content-length")
                    .and_then(|value| value.parse::<u64>().ok())
                    .ok_or(SourceError::Protocol)?;
                let strong_etag = text("etag")
                    .filter(|value| {
                        !value.starts_with("W/") && crate::model::header_text(value, 512)
                    })
                    .map(str::to_owned);
                let observed = text("x-amz-version-id");
                if version
                    .as_ref()
                    .is_some_and(|version| observed != Some(version.as_str()))
                {
                    return Err(SourceError::Changed);
                }
                if version.is_none() && strong_etag.is_none() {
                    return Err(SourceError::Protocol);
                }
                Ok(ObjectMetadata {
                    bytes,
                    strong_etag,
                    sha256: None,
                    immutable_revision: version
                        .clone()
                        .or_else(|| observed.filter(|value| *value != "null").map(str::to_owned))
                        .unwrap_or_else(|| "strong_etag".into()),
                    integrity_kind: "s3_version_or_strong_entity_validator_and_local_sha256".into(),
                })
            }
            #[cfg(feature = "hugging-face")]
            Provider::Hub { commit, .. } => hub_metadata
                .map(Ok)
                .unwrap_or_else(|| crate::hugging_face::metadata(&response.headers, commit)),
            _ => Err(SourceError::Unsupported),
        }
    }

    async fn request(
        &self,
        request: HttpRequest,
    ) -> SourceResult<(HttpResponse, Option<ObjectMetadata>)> {
        if request.url != self.virtual_url || !matches!(request.method, Method::GET | Method::HEAD)
        {
            return Err(SourceError::Denied);
        }
        let mut url = self.source.url.clone();
        #[cfg(feature = "hugging-face")]
        let mut hub_metadata: Option<ObjectMetadata> = None;
        #[cfg(not(feature = "hugging-face"))]
        let hub_metadata: Option<ObjectMetadata> = None;
        for redirect in 0..=5 {
            let remaining = self.authority.remaining()?;
            let mut headers = HeaderMap::new();
            // Only engine-generated transfer headers cross this boundary.
            for name in ["range", "if-range", "if-match", "cache-control", "accept"] {
                if let Some(value) = request.headers.get(name) {
                    headers.insert(name, value.clone());
                }
            }
            headers.insert("accept-encoding", HeaderValue::from_static("identity"));
            #[allow(unreachable_patterns)]
            match &self.source.provider {
                #[cfg(feature = "s3")]
                Provider::S3 {
                    region,
                    credentials,
                    ..
                } => {
                    if redirect != 0 {
                        return Err(SourceError::Denied);
                    }
                    if let Some(etag) = self
                        .source
                        .metadata
                        .as_ref()
                        .and_then(|value| value.strong_etag.as_ref())
                    {
                        headers.insert(
                            "if-match",
                            HeaderValue::from_str(etag).map_err(|_| SourceError::Invalid)?,
                        );
                    }
                    crate::s3::sign(
                        &request.method,
                        &url,
                        &mut headers,
                        region,
                        credentials,
                        time::OffsetDateTime::now_utc(),
                    )?;
                }
                #[cfg(feature = "hugging-face")]
                Provider::Hub { token, .. } => {
                    if !crate::hugging_face::allowed_location(&url) {
                        return Err(SourceError::Denied);
                    }
                    if url.host_str() == Some("huggingface.co") {
                        if let Some(token) = token {
                            let mut value =
                                HeaderValue::from_str(&format!("Bearer {}", token.expose()))
                                    .map_err(|_| SourceError::Credential)?;
                            value.set_sensitive(true);
                            headers.insert(http::header::AUTHORIZATION, value);
                        }
                    } else {
                        // Hub entity tags need not equal a CDN's transport validator.
                        headers.remove("if-range");
                        headers.remove("if-match");
                    }
                }
                _ => return Err(SourceError::Unsupported),
            }
            let sent = HttpRequest {
                method: request.method.clone(),
                url: url.clone(),
                headers,
                head_timeout: request.head_timeout.min(remaining),
                idle_timeout: request.idle_timeout.min(remaining),
            };
            let mut response = tokio::time::timeout(remaining, self.inner.send(sent))
                .await
                .map_err(|_| SourceError::Budget)?
                .map_err(|_| SourceError::Network)?;
            self.authority.remaining()?;
            #[cfg(feature = "hugging-face")]
            if let Provider::Hub { commit, .. } = &self.source.provider
                && url.host_str() == Some("huggingface.co")
            {
                let observed = response
                    .headers
                    .get("x-repo-commit")
                    .and_then(|value| value.to_str().ok());
                if observed.is_some_and(|value| !value.eq_ignore_ascii_case(commit)) {
                    return Err(SourceError::Changed);
                }
                if let Ok(metadata) = crate::hugging_face::metadata(&response.headers, commit) {
                    hub_metadata = Some(metadata);
                }
            }
            if response.status.is_redirection() {
                if redirect == 5 {
                    return Err(SourceError::Protocol);
                }
                let location = response
                    .headers
                    .get("location")
                    .and_then(|value| value.to_str().ok())
                    .filter(|value| value.len() <= 16384)
                    .ok_or(SourceError::Protocol)?;
                url = url.join(location).map_err(|_| SourceError::Protocol)?;
                continue;
            }
            if response.status == StatusCode::PRECONDITION_FAILED {
                return Err(SourceError::Changed);
            }
            if matches!(
                response.status,
                StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN
            ) {
                return Err(SourceError::ProviderDenied);
            }
            if response
                .headers
                .get("content-encoding")
                .is_some_and(|value| value.as_bytes() != b"identity")
            {
                return Err(SourceError::Protocol);
            }
            if let Some(metadata) = &self.source.metadata {
                if hub_metadata.as_ref().is_some_and(|current| {
                    current.bytes != metadata.bytes
                        || current.sha256 != metadata.sha256
                        || current.immutable_revision != metadata.immutable_revision
                }) {
                    return Err(SourceError::Changed);
                }
                #[cfg(feature = "s3")]
                #[cfg_attr(not(feature = "hugging-face"), allow(irrefutable_let_patterns))]
                if let Provider::S3 { version, .. } = &self.source.provider {
                    if version.as_ref().is_some_and(|version| {
                        response
                            .headers
                            .get("x-amz-version-id")
                            .and_then(|value| value.to_str().ok())
                            != Some(version.as_str())
                    }) {
                        return Err(SourceError::Changed);
                    }
                    if metadata.strong_etag.as_ref().is_some_and(|etag| {
                        response
                            .headers
                            .get("etag")
                            .and_then(|value| value.to_str().ok())
                            != Some(etag.as_str())
                    }) {
                        return Err(SourceError::Changed);
                    }
                }
                // The virtual object's validator describes the pinned provider
                // object, not any expiring signed URL chosen by its CDN.
                if let Some(etag) = &metadata.strong_etag {
                    response.headers.insert(
                        "etag",
                        HeaderValue::from_str(etag).map_err(|_| SourceError::Protocol)?,
                    );
                }
            }
            response.url = self.virtual_url.clone();
            response.headers.remove("location");
            // The daemon reviewed the final filename; provider headers cannot replace it.
            response.headers.remove("content-disposition");
            return Ok((response, hub_metadata));
        }
        Err(SourceError::Protocol)
    }
}

fn status(status: StatusCode) -> SourceError {
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => SourceError::ProviderDenied,
        StatusCode::PRECONDITION_FAILED => SourceError::Changed,
        _ => SourceError::Protocol,
    }
}
fn error(error: SourceError) -> TransportError {
    let kind = match error {
        SourceError::Network => TransportFailure::Other,
        SourceError::Protocol | SourceError::Integrity => TransportFailure::ProtocolViolation,
        _ => TransportFailure::PolicyDenied,
    };
    TransportError::new(kind, error.to_string())
}
#[async_trait]
impl HttpTransport for SourceTransport {
    async fn send(&self, request: HttpRequest) -> Result<HttpResponse, TransportError> {
        if self.source.metadata.is_none() {
            return Err(error(SourceError::Denied));
        }
        let (mut response, _) = self.request(request).await.map_err(error)?;
        let authority = self.authority.clone();
        response.body = Box::pin(stream::unfold(
            Some((response.body, authority)),
            |state| async move {
                let (mut body, authority) = state?;
                loop {
                    let remaining = match authority.remaining() {
                        Ok(value) => value,
                        Err(value) => return Some((Err(error(value)), None)),
                    };
                    let received = tokio::select! {
                        value=body.next()=>value,
                        _=tokio::time::sleep(remaining.min(Duration::from_millis(100)))=>continue,
                    };
                    return match received {
                        Some(Ok(bytes)) => match authority.charge(bytes.len() as u64) {
                            Ok(()) => Some((Ok(bytes), Some((body, authority)))),
                            Err(value) => Some((Err(error(value)), None)),
                        },
                        Some(Err(_)) => Some((Err(error(SourceError::Network)), None)),
                        None => None,
                    };
                }
            },
        ));
        Ok(response)
    }
}
