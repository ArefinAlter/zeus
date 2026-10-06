//! The workflow plan IR, version 0.
//!
//! Every field that bounds work or reach is **mandatory**. Numbers are integers (plans are hashed with JCS, which
//! forbids floats, 00 section 7). Strings that later become selectors, patterns or templates are validated by
//! [`crate::validate::validate_plan`], not at deserialisation, so a candidate plan from an agent produces a list of
//! diagnostics instead of a single parse error.

use indexmap::IndexMap;
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::{GrantScope, Origin};

use crate::canonical::{CanonicalError, jcs_of};

/// The plan format version this crate reads and writes.
pub const PLAN_VERSION: u32 = 0;

/// Identifier of a stored plan: `plan_` followed by a 26-character Crockford ULID (00 section 5.1 gains this
/// prefix; the daemon mints ids, clients only echo them).
#[derive(
    Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema,
)]
#[serde(try_from = "String", into = "String")]
#[schemars(with = "String", extend("pattern" = "^plan_[0-7][0-9A-HJKMNP-TV-Z]{25}$"))]
pub struct PlanId(String);

impl PlanId {
    /// Validate a plan id.
    pub fn parse(text: &str) -> Result<Self, String> {
        let valid = text.strip_prefix("plan_").is_some_and(|ulid| {
            ulid.len() == 26
                && ulid.starts_with(|c: char| ('0'..='7').contains(&c))
                && ulid
                    .bytes()
                    .all(|b| b"0123456789ABCDEFGHJKMNPQRSTVWXYZ".contains(&b))
        });
        if valid {
            Ok(Self(text.to_string()))
        } else {
            Err("not a plan id".to_string())
        }
    }

    /// The text form.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for PlanId {
    type Error = String;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<PlanId> for String {
    fn from(value: PlanId) -> Self {
        value.0
    }
}

impl std::fmt::Display for PlanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A complete workflow plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Plan {
    /// Must be [`PLAN_VERSION`].
    pub plan_version: u32,
    /// Short title shown in approvals and reports. Untrusted when it came from an agent.
    pub name: String,
    /// The user's goal in words, kept for the record. Untrusted: never interpreted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<String>,
    /// Hosts the run may contact: exact names (`files.example.com`) or a leading wildcard (`*.example.com`).
    /// Required and non-empty. Redirects to any other host are not followed.
    pub allowed_domains: Vec<String>,
    /// Session needs, if any. Needs a `workflow` grant approved by a human.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<SessionRequirement>,
    /// Where the crawl starts. At least one.
    pub sources: Vec<Source>,
    /// How to reach the next page of results.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pagination: Option<Pagination>,
    /// How to find items and their fields on a page.
    pub extract: Extract,
    /// Item filters, applied in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub filters: Vec<Filter>,
    /// Rate and politeness.
    pub fetch: FetchPolicy,
    /// Hard limits. Mandatory.
    pub budgets: Budgets,
    /// Naming, conversion and filing.
    #[serde(default)]
    pub post: Post,
    /// Where results go.
    pub sinks: Sinks,
    /// When to stop paginating.
    #[serde(default)]
    pub stop: StopRules,
}

impl Plan {
    /// Hex BLAKE3 of the JCS form of the plan: the "plan hash" an approval is bound to (00 section 7).
    pub fn plan_hash(&self) -> Result<String, CanonicalError> {
        let bytes = jcs_of(self)?;
        Ok(blake3::hash(&bytes).to_hex().to_string())
    }
}

/// Session requirement of a plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SessionRequirement {
    /// The origin whose session the run uses. Must be covered by `allowed_domains`.
    pub origin: Origin,
    /// Grant scope to request. Plans use `workflow` (7 days, renewable).
    #[serde(default = "default_scope")]
    pub scope: GrantScope,
}

fn default_scope() -> GrantScope {
    GrantScope::Workflow
}

/// An entry point of the crawl.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Source {
    /// Explicit start pages.
    StartUrls {
        /// Absolute http(s) URLs.
        urls: Vec<String>,
    },
    /// A sitemap (`sitemap.xml`) whose `<loc>` entries are the pages.
    Sitemap {
        /// Sitemap URL.
        url: String,
    },
    /// A JSON API endpoint that returns the items directly (use with `json_path` selectors).
    ApiEndpoint {
        /// Endpoint URL.
        url: String,
    },
}

/// How a selector locates content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Selector {
    /// CSS selector over HTML.
    Css {
        /// Selector text.
        query: String,
    },
    /// JSONPath (RFC 9535) over JSON.
    JsonPath {
        /// Expression.
        query: String,
    },
    /// Regular expression over text; capture group `group` is the result.
    Regex {
        /// Pattern (linear-time engine, no look-around).
        pattern: String,
        /// Capture group, default 1.
        #[serde(default = "one")]
        group: u32,
    },
    /// XPath. Reserved: rejected by the v0 validator.
    Xpath {
        /// Expression.
        query: String,
    },
}

fn one() -> u32 {
    1
}

/// What to take from a matched node.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Extraction {
    /// The text content.
    #[default]
    Text,
    /// An attribute value.
    Attr {
        /// Attribute name.
        name: String,
    },
}

/// One named value extracted from each item.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Field {
    /// Selector relative to the item node.
    pub selector: Selector,
    /// What to take.
    #[serde(default)]
    pub extract: Extraction,
    /// Resolve the value against the page URL into an absolute URL.
    #[serde(default)]
    pub resolve_url: bool,
    /// Items without this field are dropped (and counted in the report).
    #[serde(default)]
    pub required: bool,
}

/// Item extraction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Extract {
    /// Selector for the item nodes of a page.
    pub items: Selector,
    /// Named fields. Field names are `[a-z][a-z0-9_]{0,31}`. The field `url` is the download URL.
    pub fields: IndexMap<String, Field>,
}

/// Pagination strategy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "strategy", rename_all = "snake_case")]
pub enum Pagination {
    /// Follow `Link: <...>; rel="next"` or `<a rel="next">`.
    RelNext {
        /// Page limit for this strategy.
        max_pages: u32,
    },
    /// A numeric page parameter, for example `?page=N`.
    PageParam {
        /// Query parameter name.
        param: String,
        /// First value.
        start: u32,
        /// Increment, default 1.
        #[serde(default = "one32")]
        step: u32,
        /// Page limit.
        max_pages: u32,
    },
    /// `offset` and `limit` parameters.
    OffsetLimit {
        /// Offset parameter name.
        offset_param: String,
        /// Limit parameter name.
        limit_param: String,
        /// Page size.
        limit: u32,
        /// Page limit.
        max_pages: u32,
    },
    /// A cursor read from the JSON response and sent back as a parameter.
    Cursor {
        /// JSONPath of the next cursor in the response.
        cursor_path: String,
        /// Parameter that carries the cursor.
        param: String,
        /// Page limit.
        max_pages: u32,
    },
    /// A selector for the "next" link.
    NextSelector {
        /// Selector of the next link.
        selector: Selector,
        /// Page limit.
        max_pages: u32,
    },
    /// Infinite scroll, driven in the user's browser through the extension.
    InfiniteScroll {
        /// Maximum scroll steps.
        max_scrolls: u32,
        /// Wait after each scroll for content to settle.
        settle_ms: u32,
    },
    /// Chunk a date range into windows.
    DateRange {
        /// Parameter for the window start.
        from_param: String,
        /// Parameter for the window end.
        to_param: String,
        /// First day, `YYYY-MM-DD`.
        from: String,
        /// Last day, `YYYY-MM-DD`.
        to: String,
        /// Window length in days.
        chunk_days: u32,
    },
}

fn one32() -> u32 {
    1
}

/// An item filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Filter {
    /// Keep (or with `invert`, drop) items whose URL matches the pattern.
    UrlRegex {
        /// Pattern.
        pattern: String,
        /// Drop matches instead of keeping them.
        #[serde(default)]
        invert: bool,
    },
    /// Keep items with a declared MIME type in the list (prefix `type/*` allowed).
    Mime {
        /// Allowed types.
        allow: Vec<String>,
    },
    /// Drop items larger than this many bytes (when the size is known).
    MaxSize {
        /// Limit in bytes.
        bytes: u64,
    },
    /// Drop items smaller than this many bytes.
    MinSize {
        /// Limit in bytes.
        bytes: u64,
    },
    /// Keep items whose field matches a pattern.
    FieldRegex {
        /// Field name.
        field: String,
        /// Pattern.
        pattern: String,
    },
    /// Drop items whose key was already seen.
    Dedupe {
        /// Template that produces the key.
        key: String,
    },
}

/// Rate and politeness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct FetchPolicy {
    /// Minimum interval between requests to one host, in milliseconds. At least 200.
    pub min_interval_ms: u64,
    /// Simultaneous requests per host, 1 to 8.
    pub max_concurrency_per_host: u8,
    /// Honour robots.txt. Default and strongly recommended; switching it off needs an approval of kind
    /// `policy_exception` and is only offered for origins the user owns.
    #[serde(default = "yes")]
    pub respect_robots: bool,
}

fn yes() -> bool {
    true
}

/// Mandatory hard limits. Reaching any of them ends the run with status `budget_exhausted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Budgets {
    /// Pages fetched (listing pages and API pages).
    pub max_pages: u32,
    /// Files enqueued for download.
    pub max_files: u32,
    /// Total bytes downloaded.
    pub max_bytes: u64,
    /// Wall-clock duration of the run in milliseconds.
    pub max_duration_ms: u64,
    /// Link depth from the start pages.
    pub max_depth: u8,
}

/// Conversion step applied to downloaded files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ConvertStep {
    /// Source extensions (lowercase, no dot).
    pub from: Vec<String>,
    /// Target extension.
    pub to: String,
}

/// Post-processing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Post {
    /// Template for the file name (without directory), for example `{year}-{title|slug|truncate:60}.pdf`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rename: Option<String>,
    /// Template for a sub-directory under the destination.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subdir: Option<String>,
    /// Conversion steps (sandboxed converters, later phase).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convert: Vec<ConvertStep>,
}

/// Manifest output format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ManifestFormat {
    /// One JSON object per line (04 provenance manifest v0).
    Jsonl,
    /// Comma-separated values.
    Csv,
    /// Croissant metadata export.
    Croissant,
}

/// Manifest output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ManifestSink {
    /// Format.
    pub format: ManifestFormat,
}

/// Where results go.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Sinks {
    /// Destination directory. Must lie inside a configured write root.
    pub dest_dir: String,
    /// Manifest output, if wanted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest: Option<ManifestSink>,
}

/// When pagination stops early.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StopRules {
    /// Stop after this many consecutive pages that add no new item. 0 disables the rule.
    #[serde(default = "two")]
    pub no_new_items_pages: u32,
    /// Stop when a page repeats an earlier page's items exactly.
    #[serde(default = "yes")]
    pub stop_on_duplicate_page: bool,
    /// Selector for a "total results" hint used for estimates (never to extend budgets).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub count_hint: Option<Selector>,
}

fn two() -> u32 {
    2
}

impl Default for StopRules {
    fn default() -> Self {
        Self {
            no_new_items_pages: 2,
            stop_on_duplicate_page: true,
            count_hint: None,
        }
    }
}

#[cfg(test)]
pub(crate) fn example_plan() -> Plan {
    let mut fields = IndexMap::new();
    fields.insert(
        "url".to_string(),
        Field {
            selector: Selector::Css {
                query: "a.pdf".into(),
            },
            extract: Extraction::Attr {
                name: "href".into(),
            },
            resolve_url: true,
            required: true,
        },
    );
    fields.insert(
        "title".to_string(),
        Field {
            selector: Selector::Css { query: "h3".into() },
            extract: Extraction::Text,
            resolve_url: false,
            required: false,
        },
    );
    Plan {
        plan_version: PLAN_VERSION,
        name: "Conference papers 2026".into(),
        goal: Some("every 2026 paper PDF".into()),
        allowed_domains: vec!["portal.example.org".into()],
        session: Some(SessionRequirement {
            origin: Origin::parse("https://portal.example.org").unwrap(),
            scope: GrantScope::Workflow,
        }),
        sources: vec![Source::StartUrls {
            urls: vec!["https://portal.example.org/papers?year=2026".into()],
        }],
        pagination: Some(Pagination::PageParam {
            param: "page".into(),
            start: 1,
            step: 1,
            max_pages: 100,
        }),
        extract: Extract {
            items: Selector::Css {
                query: "li.paper".into(),
            },
            fields,
        },
        filters: vec![Filter::UrlRegex {
            pattern: r"\.pdf$".into(),
            invert: false,
        }],
        fetch: FetchPolicy {
            min_interval_ms: 2000,
            max_concurrency_per_host: 2,
            respect_robots: true,
        },
        budgets: Budgets {
            max_pages: 100,
            max_files: 2000,
            max_bytes: 10 * 1024 * 1024 * 1024,
            max_duration_ms: 7_200_000,
            max_depth: 2,
        },
        post: Post {
            rename: Some("{title|slug|truncate:60}.pdf".into()),
            subdir: None,
            convert: vec![],
        },
        sinks: Sinks {
            dest_dir: "papers".into(),
            manifest: Some(ManifestSink {
                format: ManifestFormat::Jsonl,
            }),
        },
        stop: StopRules::default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_round_trips_through_json() {
        let plan = example_plan();
        let json = serde_json::to_string(&plan).unwrap();
        let back: Plan = serde_json::from_str(&json).unwrap();
        assert_eq!(plan, back);
    }

    #[test]
    fn plan_hash_is_stable_and_sensitive() {
        let a = example_plan();
        let mut b = example_plan();
        assert_eq!(a.plan_hash().unwrap(), b.plan_hash().unwrap());
        assert_eq!(a.plan_hash().unwrap().len(), 64);
        b.budgets.max_files += 1;
        assert_ne!(a.plan_hash().unwrap(), b.plan_hash().unwrap());
    }

    #[test]
    fn budgets_are_mandatory_on_the_wire() {
        let mut value = serde_json::to_value(example_plan()).unwrap();
        value.as_object_mut().unwrap().remove("budgets");
        assert!(serde_json::from_value::<Plan>(value).is_err());
    }

    #[test]
    fn plan_ids_are_validated() {
        assert!(PlanId::parse("plan_01M45NHJ2AHWMXT8JVJ7BEEJX7").is_ok());
        for bad in [
            "plan_",
            "job_01M45NHJ2AHWMXT8JVJ7BEEJX7",
            "plan_01m45nhj2ahwmxt8jvj7beejx7",
            "plan_81M45NHJ2AHWMXT8JVJ7BEEJX7",
        ] {
            assert!(PlanId::parse(bad).is_err(), "{bad}");
        }
        let id: PlanId = serde_json::from_str("\"plan_01M45NHJ2AHWMXT8JVJ7BEEJX7\"").unwrap();
        assert_eq!(
            serde_json::to_string(&id).unwrap(),
            "\"plan_01M45NHJ2AHWMXT8JVJ7BEEJX7\""
        );
        assert!(serde_json::from_str::<PlanId>("\"nope\"").is_err());
    }

    #[test]
    fn schema_can_be_generated() {
        let schema = schemars::schema_for!(Plan);
        let text = serde_json::to_string(&schema).unwrap();
        assert!(text.contains("allowed_domains") && text.contains("budgets"));
    }
}
