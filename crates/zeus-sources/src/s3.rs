//! Native AWS S3 reads with per-method SigV4; no ambient SDK credential chain.
use std::collections::BTreeMap;

use hmac::{Hmac, KeyInit, Mac};
use http::{HeaderMap, HeaderValue, Method};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use time::OffsetDateTime;
use url::Url;
use zeroize::Zeroizing;

use crate::model::{header_text, relative_path};
use crate::{SourceAccountConfig, SourceError, SourceResult, SourceSecret, SourceTarget};

/// Explicit access key material, redacted and never serialized.
pub struct S3Credentials {
    access_key_id: SourceSecret,
    secret_access_key: SourceSecret,
    session_token: Option<SourceSecret>,
}
impl std::fmt::Debug for S3Credentials {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("S3Credentials([REDACTED])")
    }
}
impl S3Credentials {
    /// Decode a bounded, explicitly configured keyring value. Does not read files/environment.
    pub fn from_secret(value: &SourceSecret) -> SourceResult<Self> {
        #[derive(Deserialize, zeroize::Zeroize, zeroize::ZeroizeOnDrop)]
        #[serde(deny_unknown_fields)]
        struct Config {
            access_key_id: String,
            secret_access_key: String,
            session_token: Option<String>,
        }
        if value.expose().len() > 16384 {
            return Err(SourceError::Credential);
        }
        let mut config: Config =
            serde_json::from_str(value.expose()).map_err(|_| SourceError::Credential)?;
        if !header_text(&config.access_key_id, 128)
            || !header_text(&config.secret_access_key, 256)
            || config
                .session_token
                .as_ref()
                .is_some_and(|value| !header_text(value, 8192))
        {
            return Err(SourceError::Credential);
        }
        Ok(Self {
            access_key_id: SourceSecret::new(std::mem::take(&mut config.access_key_id)),
            secret_access_key: SourceSecret::new(std::mem::take(&mut config.secret_access_key)),
            session_token: config.session_token.take().map(SourceSecret::new),
        })
    }
}

/// Validate a configured native S3 account independently of a target.
pub fn validate_account(config: &SourceAccountConfig) -> SourceResult<()> {
    let SourceAccountConfig::S3 {
        region,
        bucket,
        key_prefix,
    } = config
    else {
        return Err(SourceError::Invalid);
    };
    if !(3..=63).contains(&bucket.len())
        || bucket.starts_with('-')
        || bucket.ends_with('-')
        || bucket.contains("..")
        || bucket.parse::<std::net::IpAddr>().is_ok()
        || !bucket
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || region.is_empty()
        || region.len() > 32
        || !region
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
        || (!key_prefix.is_empty() && !relative_path(key_prefix.trim_end_matches('/')))
    {
        return Err(SourceError::Invalid);
    }
    Ok(())
}

/// Build only Amazon's native HTTPS object address inside the configured bucket/prefix.
pub fn object_url(config: &SourceAccountConfig, target: &SourceTarget) -> SourceResult<Url> {
    validate_account(config)?;
    let SourceAccountConfig::S3 {
        region,
        bucket,
        key_prefix,
    } = config
    else {
        return Err(SourceError::Invalid);
    };
    let SourceTarget::S3 {
        bucket: requested,
        key,
        version_id,
    } = target
    else {
        return Err(SourceError::Invalid);
    };
    if requested != bucket
        || !relative_path(key)
        || !key.starts_with(key_prefix)
        || version_id
            .as_ref()
            .is_some_and(|value| !header_text(value, 2048) || value == "null")
    {
        return Err(SourceError::Denied);
    }
    // China regions use their distinct AWS DNS suffix; no arbitrary endpoint is accepted.
    let suffix = if region.starts_with("cn-") {
        "amazonaws.com.cn"
    } else {
        "amazonaws.com"
    };
    let mut url = Url::parse(&format!("https://{bucket}.s3.{region}.{suffix}/"))
        .map_err(|_| SourceError::Invalid)?;
    url.path_segments_mut()
        .map_err(|_| SourceError::Invalid)?
        .extend(key.split('/'));
    if let Some(version) = version_id {
        url.query_pairs_mut().append_pair("versionId", version);
    }
    Ok(url)
}

fn encode(bytes: &[u8]) -> String {
    let mut result = String::new();
    for &byte in bytes {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            result.push(byte as char)
        } else {
            result.push('%');
            result.push_str(&format!("{byte:02X}"));
        }
    }
    result
}

fn mac(key: &[u8], message: &str) -> SourceResult<Zeroizing<Vec<u8>>> {
    let mut signer = Hmac::<Sha256>::new_from_slice(key).map_err(|_| SourceError::Credential)?;
    signer.update(message.as_bytes());
    Ok(Zeroizing::new(signer.finalize().into_bytes().to_vec()))
}

/// Sign the actual HEAD or GET request, including conditional/range headers.
/// `now` is supplied explicitly so protocol vectors are deterministic.
pub fn sign(
    method: &Method,
    url: &Url,
    headers: &mut HeaderMap,
    region: &str,
    credentials: &S3Credentials,
    now: OffsetDateTime,
) -> SourceResult<()> {
    if !matches!(*method, Method::HEAD | Method::GET)
        || url.username() != ""
        || url.password().is_some()
        || url.fragment().is_some()
        || !matches!(url.scheme(), "https" | "http")
    {
        return Err(SourceError::Invalid);
    }
    let stamp = format!(
        "{:04}{:02}{:02}T{:02}{:02}{:02}Z",
        now.year(),
        u8::from(now.month()),
        now.day(),
        now.hour(),
        now.minute(),
        now.second()
    );
    let date = &stamp[..8];
    let scope = format!("{date}/{region}/s3/aws4_request");
    let hash = hex::encode(Sha256::digest([]));
    let host = url.host_str().ok_or(SourceError::Invalid)?;
    let host = match url.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_owned(),
    };
    headers.insert(
        http::header::HOST,
        HeaderValue::from_str(&host).map_err(|_| SourceError::Invalid)?,
    );
    headers.insert(
        "x-amz-date",
        HeaderValue::from_str(&stamp).map_err(|_| SourceError::Invalid)?,
    );
    headers.insert(
        "x-amz-content-sha256",
        HeaderValue::from_str(&hash).map_err(|_| SourceError::Invalid)?,
    );
    if let Some(token) = &credentials.session_token {
        let mut value =
            HeaderValue::from_str(token.expose()).map_err(|_| SourceError::Credential)?;
        value.set_sensitive(true);
        headers.insert("x-amz-security-token", value);
    }
    let mut canonical = BTreeMap::new();
    for name in [
        "host",
        "range",
        "if-match",
        "if-range",
        "x-amz-content-sha256",
        "x-amz-date",
        "x-amz-security-token",
    ] {
        if let Some(value) = headers.get(name) {
            let value = value.to_str().map_err(|_| SourceError::Invalid)?;
            canonical.insert(
                name,
                value.split_ascii_whitespace().collect::<Vec<_>>().join(" "),
            );
        }
    }
    let signed = canonical.keys().copied().collect::<Vec<_>>().join(";");
    let canonical_headers = canonical
        .iter()
        .map(|(name, value)| format!("{name}:{value}\n"))
        .collect::<String>();
    let mut query = url
        .query_pairs()
        .map(|(key, value)| (encode(key.as_bytes()), encode(value.as_bytes())))
        .collect::<Vec<_>>();
    query.sort();
    let query = query
        .into_iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect::<Vec<_>>()
        .join("&");
    let canonical_request = format!(
        "{method}\n{}\n{query}\n{canonical_headers}\n{signed}\n{hash}",
        url.path()
    );
    let to_sign = format!(
        "AWS4-HMAC-SHA256\n{stamp}\n{scope}\n{}",
        hex::encode(Sha256::digest(canonical_request.as_bytes()))
    );
    let key = Zeroizing::new(format!("AWS4{}", credentials.secret_access_key.expose()));
    let date_key = mac(key.as_bytes(), date)?;
    let region_key = mac(&date_key, region)?;
    let service_key = mac(&region_key, "s3")?;
    let signing_key = mac(&service_key, "aws4_request")?;
    let signature = hex::encode(&*mac(&signing_key, &to_sign)?);
    let authorization = Zeroizing::new(format!(
        "AWS4-HMAC-SHA256 Credential={}/{scope},SignedHeaders={signed},Signature={signature}",
        credentials.access_key_id.expose()
    ));
    let mut value = HeaderValue::from_str(&authorization).map_err(|_| SourceError::Credential)?;
    value.set_sensitive(true);
    headers.insert(http::header::AUTHORIZATION, value);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn published_aws_s3_get_range_signature_vector() {
        // AWS's published S3 SigV4 header-auth GET example,2013-05-24.
        let credentials = S3Credentials::from_secret(&SourceSecret::new(
            r#"{"access_key_id":"AKIAIOSFODNN7EXAMPLE","secret_access_key":"wJalrXUtnFEMI/K7MDENG/bPxRfiCYEXAMPLEKEY"}"#.into(),
        )).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert("range", "bytes=0-9".parse().unwrap());
        sign(
            &Method::GET,
            &Url::parse("https://examplebucket.s3.amazonaws.com/test.txt").unwrap(),
            &mut headers,
            "us-east-1",
            &credentials,
            OffsetDateTime::from_unix_timestamp(1369353600).unwrap(),
        )
        .unwrap();
        assert!(headers["authorization"].to_str().unwrap().ends_with(
            "Signature=f0e8bdb87c964420e857bd35b5d6ed310bd44f0170aba48dd91039c6036bdb41"
        ));
    }
    #[test]
    fn signing_method_range_and_session_are_bound_without_secret_debug() {
        let credential=SourceSecret::new(r#"{"access_key_id":"ZEUS-CANARY-access","secret_access_key":"ZEUS-CANARY-secret","session_token":"ZEUS-CANARY-session"}"#.into());
        let credential = S3Credentials::from_secret(&credential).unwrap();
        let url =
            Url::parse("https://examplebucket.s3.us-east-1.amazonaws.com/test.txt?versionId=a%2Fb")
                .unwrap();
        let now = OffsetDateTime::from_unix_timestamp(1369353600).unwrap();
        let mut head = HeaderMap::new();
        sign(
            &Method::HEAD,
            &url,
            &mut head,
            "us-east-1",
            &credential,
            now,
        )
        .unwrap();
        let mut get = HeaderMap::new();
        get.insert("range", "bytes=0-9".parse().unwrap());
        sign(&Method::GET, &url, &mut get, "us-east-1", &credential, now).unwrap();
        assert_ne!(head["authorization"], get["authorization"]);
        assert!(get["authorization"].to_str().unwrap().contains(";range;"));
        assert!(get["authorization"].is_sensitive());
        assert!(!format!("{credential:?} {get:?}").contains("ZEUS-CANARY"));
    }
    #[test]
    fn account_scope_is_exact_and_paths_cannot_normalize_to_another_object() {
        let config = SourceAccountConfig::S3 {
            region: "us-east-1".into(),
            bucket: "examplebucket".into(),
            key_prefix: "allowed/".into(),
        };
        let target = |key: &str| SourceTarget::S3 {
            bucket: "examplebucket".into(),
            key: key.into(),
            version_id: Some("a/b+?".into()),
        };
        assert!(object_url(&config, &target("allowed/file")).is_ok());
        assert!(object_url(&config, &target("allowed/../secret")).is_err());
        assert!(object_url(&config, &target("allowed-other/file")).is_err());
    }
}
