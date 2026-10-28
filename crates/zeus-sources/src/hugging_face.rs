//! Immutable Hub file addressing and conservative publisher-digest semantics.
use http::HeaderMap;
use url::Url;

use crate::model::relative_path;
use crate::{
    HubRepositoryType, ObjectMetadata, SourceAccountConfig, SourceError, SourceResult, SourceTarget,
};

fn repository(value: &str) -> bool {
    let mut parts = value.split('/');
    let valid = |part: &str| {
        !part.is_empty()
            && part.len() <= 96
            && part != "."
            && part != ".."
            && part
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || b"-_.".contains(&byte))
    };
    parts.next().is_some_and(valid) && parts.next().is_some_and(valid) && parts.next().is_none()
}

/// Validate the exact repository restriction without network or ambient token reads.
pub fn validate_account(config: &SourceAccountConfig) -> SourceResult<()> {
    match config {
        SourceAccountConfig::HuggingFace {
            repository: value, ..
        } if repository(value) => Ok(()),
        _ => Err(SourceError::Invalid),
    }
}

/// Build a canonical full-commit resolve URL within the configured repository.
pub fn object_url(config: &SourceAccountConfig, target: &SourceTarget) -> SourceResult<Url> {
    validate_account(config)?;
    let SourceAccountConfig::HuggingFace {
        repository_type: allowed_type,
        repository: allowed,
    } = config
    else {
        return Err(SourceError::Invalid);
    };
    let SourceTarget::HuggingFace {
        repository_type,
        repository,
        commit,
        path,
    } = target
    else {
        return Err(SourceError::Invalid);
    };
    if repository_type != allowed_type
        || repository != allowed
        || commit.len() != 40
        || !commit.bytes().all(|byte| byte.is_ascii_hexdigit())
        || !relative_path(path)
    {
        return Err(SourceError::Denied);
    }
    let mut url = Url::parse("https://huggingface.co/").map_err(|_| SourceError::Invalid)?;
    {
        let mut parts = url.path_segments_mut().map_err(|_| SourceError::Invalid)?;
        match repository_type {
            HubRepositoryType::Model => {}
            HubRepositoryType::Dataset => {
                parts.push("datasets");
            }
            HubRepositoryType::Space => {
                parts.push("spaces");
            }
        }
        parts
            .extend(repository.split('/'))
            .push("resolve")
            .push(&commit.to_ascii_lowercase())
            .extend(path.split('/'));
    }
    Ok(url)
}

/// Only documented Hub/CDN DNS families can receive the Hub redirect. Authorization
/// is attached solely to huggingface.co, never to these CDN hosts.
pub fn allowed_location(url: &Url) -> bool {
    let host = url.host_str().unwrap_or("");
    url.scheme() == "https"
        && url.username().is_empty()
        && url.password().is_none()
        && url.port().is_none_or(|port| port == 443)
        && (host == "huggingface.co"
            || host == "hf.co"
            || host.ends_with(".hf.co")
            || host == "huggingfaceusercontent.com"
            || host.ends_with(".huggingfaceusercontent.com")
            || host == "cdn-lfs.huggingface.co"
            || host.ends_with(".cdn-lfs.huggingface.co"))
}

/// Interpret Hub metadata, preserving Git-blob versus LFS whole-file semantics.
pub fn metadata(headers: &HeaderMap, commit: &str) -> SourceResult<ObjectMetadata> {
    let text = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
    if text("x-repo-commit").is_some_and(|observed| !observed.eq_ignore_ascii_case(commit)) {
        return Err(SourceError::Changed);
    }
    let bytes = text("x-linked-size")
        .or_else(|| text("content-length"))
        .and_then(|value| value.parse::<u64>().ok())
        .ok_or(SourceError::Protocol)?;
    let etag = text("x-linked-etag").or_else(|| text("etag"));
    let raw = etag.map(|value| value.trim_matches('"'));
    let sha256 = raw
        .filter(|value| value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .map(str::to_ascii_lowercase);
    let integrity_kind = if sha256.is_some() {
        "hub_lfs_whole_sha256"
    } else if raw.is_some_and(|value| {
        value.len() == 40 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
    }) {
        "hub_git_blob_sha1_not_whole_sha256"
    } else {
        "commit_pin_and_local_sha256"
    };
    // Never call a weak ETag an entity lock. The immutable commit still binds
    // public Hub file resolution; downstream CDN validators are kept separate.
    let strong_etag = etag
        .filter(|value| !value.starts_with("W/") && value.len() <= 512)
        .map(|value| {
            if value.starts_with('"') {
                value.to_owned()
            } else {
                format!("\"{value}\"")
            }
        });
    Ok(ObjectMetadata {
        bytes,
        strong_etag,
        sha256,
        immutable_revision: commit.to_ascii_lowercase(),
        integrity_kind: integrity_kind.into(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn only_full_commits_and_exact_repository_scopes_are_accepted() {
        let config = SourceAccountConfig::HuggingFace {
            repository_type: HubRepositoryType::Dataset,
            repository: "owner/data".into(),
        };
        let target = |commit: &str, path: &str| SourceTarget::HuggingFace {
            repository_type: HubRepositoryType::Dataset,
            repository: "owner/data".into(),
            commit: commit.into(),
            path: path.into(),
        };
        let commit = "a".repeat(40);
        assert_eq!(
            object_url(&config, &target(&commit, "dir/a b.bin"))
                .unwrap()
                .path(),
            format!("/datasets/owner/data/resolve/{commit}/dir/a%20b.bin")
        );
        assert!(object_url(&config, &target("main", "x")).is_err());
        assert!(object_url(&config, &target(&commit, "a/../private")).is_err());
        for url in [
            "https://cas-bridge.xethub.hf.co/x?token=private",
            "https://cdn-lfs.huggingface.co/x",
        ] {
            assert!(allowed_location(&Url::parse(url).unwrap()));
        }
        for url in [
            "https://hf.co.evil.invalid/x",
            "http://cdn-lfs.huggingface.co/x",
            "https://huggingface.co@localhost/x",
        ] {
            assert!(!allowed_location(&Url::parse(url).unwrap()));
        }
    }
    #[test]
    fn git_blob_and_lfs_checksums_have_distinct_semantics() {
        let commit = "a".repeat(40);
        let mut headers = HeaderMap::new();
        headers.insert("content-length", "7".parse().unwrap());
        headers.insert("x-repo-commit", commit.parse().unwrap());
        headers.insert("x-linked-etag", "b".repeat(40).parse().unwrap());
        let git = metadata(&headers, &commit).unwrap();
        assert!(git.sha256.is_none());
        assert!(git.integrity_kind.contains("git_blob"));
        headers.insert("x-linked-etag", "c".repeat(64).parse().unwrap());
        assert_eq!(
            metadata(&headers, &commit).unwrap().sha256,
            Some("c".repeat(64))
        );
        headers.insert("x-repo-commit", "d".repeat(40).parse().unwrap());
        assert_eq!(metadata(&headers, &commit), Err(SourceError::Changed));
    }
}
