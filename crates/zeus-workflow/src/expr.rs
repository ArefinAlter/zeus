//! The only expression language in plans: templates.
//!
//! ```text
//! template    = { literal | placeholder }
//! placeholder = "{" field { "|" filter } "}"
//! filter      = name [ ":" argument ]
//! ```
//!
//! `{{` and `}}` are literal braces. Fields are `[a-z][a-z0-9_]{0,31}`. The filter set is closed:
//! `lower`, `upper`, `trim`, `slug`, `strip_ext`, `truncate:N` (1 to 255 characters) and `default:TEXT`.
//! There are no loops, conditionals, calls or I/O. Limits: at most 512 bytes, 16 placeholders, 4 filters each.

use std::collections::{BTreeMap, BTreeSet};

use thiserror::Error;

/// Longest template in bytes.
pub const MAX_TEMPLATE_BYTES: usize = 512;
/// Most placeholders in one template.
pub const MAX_PLACEHOLDERS: usize = 16;
/// Most filters on one placeholder.
pub const MAX_FILTERS: usize = 4;
/// Longest `truncate` argument.
pub const MAX_TRUNCATE: usize = 255;
/// Longest `default` argument in characters.
pub const MAX_DEFAULT_CHARS: usize = 64;

/// Why a template failed to parse or render.
#[derive(Debug, Error, PartialEq, Eq)]
pub enum ExprError {
    /// The template is longer than [`MAX_TEMPLATE_BYTES`].
    #[error("template is longer than {MAX_TEMPLATE_BYTES} bytes")]
    TooLong,
    /// More than [`MAX_PLACEHOLDERS`] placeholders.
    #[error("more than {MAX_PLACEHOLDERS} placeholders")]
    TooManyPlaceholders,
    /// A `{` without a matching `}` or a stray `}`.
    #[error("unbalanced brace at byte {0}")]
    Unbalanced(usize),
    /// A field name that does not match `[a-z][a-z0-9_]{{0,31}}`.
    #[error("invalid field name `{0}`")]
    InvalidField(String),
    /// A filter that is not in the closed set.
    #[error("unknown filter `{0}`")]
    UnknownFilter(String),
    /// A filter argument that is missing, extra or out of range.
    #[error("invalid argument for filter `{0}`")]
    InvalidFilterArgument(String),
    /// More than [`MAX_FILTERS`] filters on one placeholder.
    #[error("more than {MAX_FILTERS} filters on one placeholder")]
    TooManyFilters,
    /// Rendering needed a field that has no value and no `default` filter.
    #[error("missing value for field `{0}`")]
    MissingValue(String),
}

/// A filter applied to a field value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TemplateFilter {
    /// Lowercase.
    Lower,
    /// Uppercase.
    Upper,
    /// Trim surrounding whitespace.
    Trim,
    /// Lowercase, runs of non-alphanumerics become one `-`.
    Slug,
    /// Remove a trailing `.ext` of at most 8 alphanumeric characters.
    StripExt,
    /// Keep at most N characters.
    Truncate(usize),
    /// Use TEXT when the value is missing or empty.
    Default(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Part {
    Literal(String),
    Placeholder {
        field: String,
        filters: Vec<TemplateFilter>,
    },
}

/// A parsed template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Template {
    parts: Vec<Part>,
}

fn valid_field(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

fn parse_filter(spec: &str) -> Result<TemplateFilter, ExprError> {
    let (name, arg) = match spec.split_once(':') {
        Some((n, a)) => (n, Some(a)),
        None => (spec, None),
    };
    let no_arg = |f: TemplateFilter| match arg {
        None => Ok(f),
        Some(_) => Err(ExprError::InvalidFilterArgument(name.to_string())),
    };
    match name {
        "lower" => no_arg(TemplateFilter::Lower),
        "upper" => no_arg(TemplateFilter::Upper),
        "trim" => no_arg(TemplateFilter::Trim),
        "slug" => no_arg(TemplateFilter::Slug),
        "strip_ext" => no_arg(TemplateFilter::StripExt),
        "truncate" => {
            let n: usize = arg
                .and_then(|a| a.parse().ok())
                .filter(|n| (1..=MAX_TRUNCATE).contains(n))
                .ok_or_else(|| ExprError::InvalidFilterArgument(name.to_string()))?;
            Ok(TemplateFilter::Truncate(n))
        }
        "default" => {
            let text = arg.ok_or_else(|| ExprError::InvalidFilterArgument(name.to_string()))?;
            if text.chars().count() > MAX_DEFAULT_CHARS || text.chars().any(|c| c.is_control()) {
                return Err(ExprError::InvalidFilterArgument(name.to_string()));
            }
            Ok(TemplateFilter::Default(text.to_string()))
        }
        other => Err(ExprError::UnknownFilter(other.to_string())),
    }
}

impl Template {
    /// Parse and validate a template.
    pub fn parse(text: &str) -> Result<Self, ExprError> {
        if text.len() > MAX_TEMPLATE_BYTES {
            return Err(ExprError::TooLong);
        }
        let mut parts = Vec::new();
        let mut literal = String::new();
        let mut placeholders = 0usize;
        let bytes = text.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            match bytes[i] {
                b'{' if bytes.get(i + 1) == Some(&b'{') => {
                    literal.push('{');
                    i += 2;
                }
                b'}' if bytes.get(i + 1) == Some(&b'}') => {
                    literal.push('}');
                    i += 2;
                }
                b'{' => {
                    let close = text[i + 1..].find(['}', '{']).map(|p| i + 1 + p);
                    let Some(end) = close.filter(|&p| bytes[p] == b'}') else {
                        return Err(ExprError::Unbalanced(i));
                    };
                    placeholders += 1;
                    if placeholders > MAX_PLACEHOLDERS {
                        return Err(ExprError::TooManyPlaceholders);
                    }
                    let inner = &text[i + 1..end];
                    let mut pieces = inner.split('|');
                    let field = pieces.next().unwrap_or_default();
                    if !valid_field(field) {
                        return Err(ExprError::InvalidField(field.to_string()));
                    }
                    let filters = pieces.map(parse_filter).collect::<Result<Vec<_>, _>>()?;
                    if filters.len() > MAX_FILTERS {
                        return Err(ExprError::TooManyFilters);
                    }
                    if !literal.is_empty() {
                        parts.push(Part::Literal(std::mem::take(&mut literal)));
                    }
                    parts.push(Part::Placeholder {
                        field: field.to_string(),
                        filters,
                    });
                    i = end + 1;
                }
                b'}' => return Err(ExprError::Unbalanced(i)),
                _ => {
                    // Copy one full UTF-8 character.
                    let ch = text[i..].chars().next().unwrap_or('\u{fffd}');
                    literal.push(ch);
                    i += ch.len_utf8();
                }
            }
        }
        if !literal.is_empty() {
            parts.push(Part::Literal(literal));
        }
        Ok(Self { parts })
    }

    /// The field names the template reads.
    pub fn fields(&self) -> BTreeSet<&str> {
        self.parts
            .iter()
            .filter_map(|p| match p {
                Part::Placeholder { field, .. } => Some(field.as_str()),
                Part::Literal(_) => None,
            })
            .collect()
    }

    /// Render with the given values.
    pub fn render(&self, values: &BTreeMap<String, String>) -> Result<String, ExprError> {
        let mut out = String::new();
        for part in &self.parts {
            match part {
                Part::Literal(text) => out.push_str(text),
                Part::Placeholder { field, filters } => {
                    let mut value = values.get(field).cloned();
                    for filter in filters {
                        if value.is_none() && !matches!(filter, TemplateFilter::Default(_)) {
                            return Err(ExprError::MissingValue(field.clone()));
                        }
                        value = Some(apply(filter, value));
                    }
                    match value {
                        Some(v) => out.push_str(&v),
                        None => return Err(ExprError::MissingValue(field.clone())),
                    }
                }
            }
        }
        Ok(out)
    }
}

fn apply(filter: &TemplateFilter, value: Option<String>) -> String {
    if let TemplateFilter::Default(text) = filter {
        return match value {
            Some(v) if !v.is_empty() => v,
            _ => text.clone(),
        };
    }
    let value = value.unwrap_or_default();
    match filter {
        TemplateFilter::Lower => value.to_lowercase(),
        TemplateFilter::Upper => value.to_uppercase(),
        TemplateFilter::Trim => value.trim().to_string(),
        TemplateFilter::Slug => slug(&value),
        TemplateFilter::StripExt => strip_ext(&value),
        TemplateFilter::Truncate(n) => value.chars().take(*n).collect(),
        TemplateFilter::Default(_) => value,
    }
}

fn slug(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    let mut pending_dash = false;
    for c in value.chars() {
        if c.is_alphanumeric() {
            if pending_dash && !out.is_empty() {
                out.push('-');
            }
            pending_dash = false;
            out.extend(c.to_lowercase());
        } else {
            pending_dash = true;
        }
    }
    out
}

fn strip_ext(value: &str) -> String {
    match value.rsplit_once('.') {
        Some((stem, ext))
            if !stem.is_empty()
                && ext.len() <= 8
                && ext.chars().all(|c| c.is_ascii_alphanumeric()) =>
        {
            stem.to_string()
        }
        _ => value.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn renders_the_research_example() {
        let t = Template::parse("{year}-{first_author|slug}-{title|slug|truncate:20}.pdf").unwrap();
        let out = t
            .render(&values(&[
                ("year", "2026"),
                ("first_author", "Ada Lovelace"),
                ("title", "On Engines, Analytical & Otherwise"),
            ]))
            .unwrap();
        assert_eq!(out, "2026-ada-lovelace-on-engines-analytica.pdf");
    }

    #[test]
    fn braces_can_be_escaped() {
        let t = Template::parse("{{literal}} {a}").unwrap();
        assert_eq!(t.render(&values(&[("a", "x")])).unwrap(), "{literal} x");
    }

    #[test]
    fn default_filter_covers_missing_values() {
        let t = Template::parse("{a|default:unknown}").unwrap();
        assert_eq!(t.render(&BTreeMap::new()).unwrap(), "unknown");
        assert_eq!(t.render(&values(&[("a", "")])).unwrap(), "unknown");
        let t = Template::parse("{a}").unwrap();
        assert_eq!(
            t.render(&BTreeMap::new()),
            Err(ExprError::MissingValue("a".into()))
        );
    }

    #[test]
    fn rejects_everything_that_is_not_a_template() {
        for bad in [
            "{",
            "}",
            "{a",
            "{A}",
            "{a b}",
            "{a|nope}",
            "{a|truncate}",
            "{a|truncate:0}",
            "{a|lower:1}",
            "{a|trim|trim|trim|trim|trim}",
            "{1a}",
            "{a{b}}",
        ] {
            assert!(Template::parse(bad).is_err(), "{bad}");
        }
        assert_eq!(Template::parse(&"x".repeat(513)), Err(ExprError::TooLong));
        assert_eq!(
            Template::parse(&"{a}".repeat(17)),
            Err(ExprError::TooManyPlaceholders)
        );
    }

    #[test]
    fn code_injection_attempts_are_inert_text_or_errors() {
        // No expression syntax beyond placeholders exists: these are either literals or parse errors.
        assert!(Template::parse("{a.__class__}").is_err());
        assert!(Template::parse("{system(\"x\")}").is_err());
        let t = Template::parse("$(rm -rf /) `x` ${a}").unwrap();
        assert_eq!(t.fields().into_iter().collect::<Vec<_>>(), ["a"]);
        let t = Template::parse("$(rm -rf /) `x`").unwrap();
        assert_eq!(t.render(&BTreeMap::new()).unwrap(), "$(rm -rf /) `x`");
    }

    #[test]
    fn fields_are_listed() {
        let t = Template::parse("{b}/{a|lower}/{b}").unwrap();
        assert_eq!(t.fields().into_iter().collect::<Vec<_>>(), ["a", "b"]);
    }

    proptest! {
        #[test]
        fn parse_never_panics(text in ".{0,600}") {
            let _ = Template::parse(&text);
        }

        #[test]
        fn literal_text_without_braces_round_trips(text in "[^{}]{0,200}") {
            let t = Template::parse(&text).unwrap();
            prop_assert_eq!(t.render(&BTreeMap::new()).unwrap(), text);
        }

        #[test]
        fn slug_output_is_url_safe(value in ".{0,80}") {
            let out = slug(&value);
            prop_assert!(!out.starts_with('-') && !out.ends_with('-'));
            prop_assert!(!out.contains("--"));
        }
    }
}
