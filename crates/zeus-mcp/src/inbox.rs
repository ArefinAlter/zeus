//! Closed owner-scoped intake operations; human inspection selectors and approval methods are absent.
use crate::{
    server::ToolOutcome,
    workspace::{self, WorkspaceOutput},
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use zeus_api::{
    ApiError, ApiResult, Client, call, methods::inbox::*, methods::*, paging::PageRequest,
};
/// On-demand parameter schemas keep the always-present tool catalogue small.
pub const SCHEMA_URI: &str = "zeus://inbox/schema";
/// One durable operation intent. Parameter fields are decoded through the corresponding API type.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxArgs {
    /// Reuse only for the identical operation;1–128 UTF-8 bytes without controls.
    pub idempotency_key: String,
    /// Closed action and typed API parameters, described by zeus://inbox/schema.
    pub request: InboxRequest,
}
/// One action, never an arbitrary RPC method.
#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct InboxRequest {
    /// Operation in the intake namespace.
    pub action: Action,
    /// Parameter schema is available on demand; owner selectors are not accepted.
    #[serde(default)]
    pub parameters: Map<String, Value>,
}
/// Implemented owner-scoped operations only.
#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Action {
    /// Create/update a package.
    PackagePut,
    /// Page through owned packages.
    PackagesList,
    /// Read owned package metadata and entries.
    PackageGet,
    /// Archive/unarchive metadata; existing downloads remain.
    PackageArchive,
    /// Persist up to200 exact requests with deduplication.
    EntriesAdd,
    /// Remove pending metadata without canceling admitted jobs.
    EntriesRemove,
    /// Retain an immutable at-most50-entry destination/collision preview.
    Preview,
    /// Dispatch only the exact saved preview; daemon session approvals remain independent.
    Submit,
    /// Read one durable batch's state.
    SubmissionGet,
    /// Reconcile the same saved batch rather than constructing a duplicate enqueue.
    SubmissionRetry,
}
impl Action {
    fn name(self) -> &'static str {
        match self {
            Self::PackagePut => "package_put",
            Self::PackagesList => "packages_list",
            Self::PackageGet => "package_get",
            Self::PackageArchive => "package_archive",
            Self::EntriesAdd => "entries_add",
            Self::EntriesRemove => "entries_remove",
            Self::Preview => "preview",
            Self::Submit => "submit",
            Self::SubmissionGet => "submission_get",
            Self::SubmissionRetry => "submission_retry",
        }
    }
    fn schema(self) -> Value {
        macro_rules! schema {
            ($t:ty) => {
                serde_json::to_value(schemars::schema_for!($t))
                    .expect("static intake parameter schema")
            };
        }
        let mut schema = match self {
            Self::PackagePut => schema!(InboxPackagePutParams),
            Self::PackagesList => schema!(InboxPackagesListParams),
            Self::PackageGet => schema!(InboxPackageGetParams),
            Self::PackageArchive => schema!(InboxPackageArchiveParams),
            Self::EntriesAdd => schema!(InboxEntriesAddParams),
            Self::EntriesRemove => schema!(InboxEntriesRemoveParams),
            Self::Preview => schema!(InboxPreviewParams),
            Self::Submit => schema!(InboxSubmitParams),
            Self::SubmissionGet => schema!(InboxSubmissionGetParams),
            Self::SubmissionRetry => schema!(InboxSubmissionRetryParams),
        };
        if let Some(properties) = schema["properties"].as_object_mut() {
            properties.remove("idempotency_key");
            properties.remove("owner_credential_id");
        }
        if let Some(required) = schema["required"].as_array_mut() {
            required.retain(|k| k != "idempotency_key" && k != "owner_credential_id");
        }
        schema["additionalProperties"] = json!(false);
        schema
    }
}
/// Each schema is standalone; the outer MCP idempotency key is injected only for mutations.
pub fn request_schemas() -> Value {
    let actions = [
        Action::PackagePut,
        Action::PackagesList,
        Action::PackageGet,
        Action::PackageArchive,
        Action::EntriesAdd,
        Action::EntriesRemove,
        Action::Preview,
        Action::Submit,
        Action::SubmissionGet,
        Action::SubmissionRetry,
    ];
    let schemas: Map<String, Value> = actions
        .into_iter()
        .map(|a| (a.name().into(), a.schema()))
        .collect();
    json!({"version":1,"schemas":schemas,"note":"Parameters never accept owner selectors. Read-only human inspection belongs to the trusted CLI. Source URLs and headers remain private; tool replies are redacted. Use small page limits for the32KiB tool budget."})
}
fn fail(error: ApiError) -> ToolOutcome {
    ToolOutcome::from_api(&error)
}
/// Decode one closed action, preserving exact daemon idempotency/visibility/preview enforcement.
pub async fn dispatch<C: Client>(
    client: &C,
    mut args: InboxArgs,
) -> Result<WorkspaceOutput, ToolOutcome> {
    workspace::key(&args.idempotency_key)?;
    if args.request.parameters.contains_key("owner_credential_id") {
        return Err(fail(ApiError::invalid_params(
            "MCP intake cannot select another owner",
        )));
    }
    let schema = args.request.action.schema();
    let properties = schema["properties"]
        .as_object()
        .expect("intake parameters are objects");
    if args
        .request
        .parameters
        .keys()
        .any(|k| !properties.contains_key(k))
    {
        return Err(fail(ApiError::invalid_params(
            "unknown intake operation parameter",
        )));
    }
    if matches!(
        args.request.action,
        Action::PackageGet | Action::PackagesList
    ) && !args.request.parameters.contains_key("limit")
    {
        args.request.parameters.insert("limit".into(), json!(10));
    }
    let params = Value::Object(args.request.parameters);
    let key = &args.idempotency_key;
    macro_rules! rpc {
        ($m:ty,$name:literal) => {
            ($name, workspace::rpc::<$m, _>(client, params, key).await?)
        };
    }
    let (method, data) = match args.request.action {
        Action::PackagePut => rpc!(InboxPackagePut, "inbox.packages.put"),
        Action::PackagesList => rpc!(InboxPackagesList, "inbox.packages.list"),
        Action::PackageGet => rpc!(InboxPackageGet, "inbox.packages.get"),
        Action::PackageArchive => rpc!(InboxPackageArchive, "inbox.packages.archive"),
        Action::EntriesAdd => rpc!(InboxEntriesAdd, "inbox.entries.add"),
        Action::EntriesRemove => rpc!(InboxEntriesRemove, "inbox.entries.remove"),
        Action::Preview => rpc!(InboxPreview, "inbox.preview"),
        Action::Submit => rpc!(InboxSubmit, "inbox.submit"),
        Action::SubmissionGet => rpc!(InboxSubmissionGet, "inbox.submissions.get"),
        Action::SubmissionRetry => rpc!(InboxSubmissionRetry, "inbox.submissions.retry"),
    };
    let next = if matches!(args.request.action, Action::Preview) {
        "review_plan"
    } else if data["entries"].as_array().is_some_and(|entries| {
        entries
            .iter()
            .any(|entry| entry["result"]["approval_id"].is_string())
    }) {
        "ask_user"
    } else {
        "none"
    };
    // Preserve a small typed recovery handle even when a committed mutation's full projection
    // exceeds the tool budget. Never silently drop retained IDs or create a replacement intent.
    let package = data
        .get("package_id")
        .or_else(|| data.get("package").and_then(|p| p.get("id")))
        .and_then(Value::as_str)
        .and_then(|id| id.parse::<InboxPackageId>().ok());
    let batch = data
        .get("batch_id")
        .and_then(Value::as_str)
        .and_then(|id| id.parse::<InboxBatchId>().ok());
    match workspace::output(method, data, next) {
        Err(ToolOutcome::Error { envelope, .. })
            if envelope.code.as_str() == "api.payload_too_large" =>
        {
            let mut error = ApiError::new(
                "api.payload_too_large",
                "Intake result exceeds32KiB; a mutation may have committed. Preserve its original idempotency key and inspect the retained package or submission resource.",
            );
            if let Some(package) = package {
                let uri = match batch {
                    Some(batch) => format!("zeus://inbox/packages/{package}/submissions/{batch}"),
                    None => format!("zeus://inbox/packages/{package}?limit=10"),
                };
                error = error.with_detail("resource_uri", uri);
            }
            Err(fail(error))
        }
        result => result,
    }
}
/// A typed resource reference; only owner-scoped read operations are represented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InboxResource {
    /// Static parameter schemas.
    Schema,
    /// One bounded entry page with history disabled.
    Package {
        /// Owned package.
        id: InboxPackageId,
        /// Page size,1–100.
        limit: u32,
        /// Opaque owner/revision-bound continuation.
        cursor: Option<String>,
    },
    /// Exact batch receipt, without hidden dispatch payloads.
    Submission {
        /// Owned package.
        package: InboxPackageId,
        /// Saved dispatch.
        batch: InboxBatchId,
    },
}
impl InboxResource {
    /// Parse only registered resources; query credentials/owner selectors and unknown keys are refused.
    pub fn parse(raw: &str) -> Option<Self> {
        let uri = url::Url::parse(raw).ok()?;
        if uri.scheme() != "zeus"
            || uri.host_str() != Some("inbox")
            || !uri.username().is_empty()
            || uri.password().is_some()
            || uri.port().is_some()
            || uri.fragment().is_some()
        {
            return None;
        }
        let parts: Vec<_> = uri.path().trim_start_matches('/').split('/').collect();
        if parts == ["schema"] && uri.query().is_none() {
            return Some(Self::Schema);
        }
        if let ["packages", id] = parts.as_slice() {
            let mut limit = 10;
            let mut cursor = None;
            let mut seen = std::collections::BTreeSet::new();
            for (k, v) in uri.query_pairs() {
                if !seen.insert(k.to_string()) {
                    return None;
                }
                match k.as_ref() {
                    "limit" => {
                        limit = v.parse().ok()?;
                        if !(1..=100).contains(&limit) {
                            return None;
                        }
                    }
                    "cursor" if v.len() <= 4096 => cursor = Some(v.into_owned()),
                    _ => return None,
                }
            }
            return Some(Self::Package {
                id: id.parse().ok()?,
                limit,
                cursor,
            });
        }
        if let ["packages", package, "submissions", batch] = parts.as_slice()
            && uri.query().is_none()
        {
            return Some(Self::Submission {
                package: package.parse().ok()?,
                batch: batch.parse().ok()?,
            });
        }
        None
    }
}
/// Read bounded redacted data through the same owner-scoped API as the tool.
pub async fn read<C: Client>(client: &C, resource: &InboxResource) -> ApiResult<String> {
    let value = match resource {
        InboxResource::Schema => request_schemas(),
        InboxResource::Package { id, limit, cursor } => serde_json::to_value(
            call::<InboxPackageGet, _>(
                client,
                InboxPackageGetParams {
                    package_id: id.clone(),
                    include_history: false,
                    owner_credential_id: None,
                    page: PageRequest {
                        limit: Some(*limit),
                        cursor: cursor.clone(),
                    },
                    pending_only: false,
                },
            )
            .await?,
        )
        .map_err(|_| ApiError::internal("serialize package resource"))?,
        InboxResource::Submission { package, batch } => serde_json::to_value(
            call::<InboxSubmissionGet, _>(
                client,
                InboxSubmissionGetParams {
                    package_id: package.clone(),
                    batch_id: batch.clone(),
                    owner_credential_id: None,
                },
            )
            .await?,
        )
        .map_err(|_| ApiError::internal("serialize submission resource"))?,
    };
    let text = serde_json::to_string_pretty(&value)
        .map_err(|_| ApiError::internal("serialize intake resource"))?;
    if text.len() > 1_048_576 {
        return Err(ApiError::new(
            zeus_api::error::codes::PAYLOAD_TOO_LARGE,
            "intake resource exceeds1MiB; request a smaller entry page",
        ));
    }
    Ok(text)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn schemas_have_no_human_selector_and_resources_cannot_select_another_owner() {
        let schema = request_schemas();
        let text = serde_json::to_string(&schema).unwrap();
        assert!(!text.contains("owner_credential_id"));
        assert!(InboxResource::parse(SCHEMA_URI).is_some());
        let id = InboxPackageId::from_job_id(zeus_types::IdGenerator::new().job_id());
        assert!(InboxResource::parse(&format!("zeus://inbox/packages/{id}?limit=1")).is_some());
        for q in [
            "owner_credential_id=cli_other",
            "token=secret",
            "limit=0",
            "limit=101",
            "limit=1&limit=2",
            "cursor=x&cursor=y",
        ] {
            assert!(InboxResource::parse(&format!("zeus://inbox/packages/{id}?{q}")).is_none());
        }
    }
}
