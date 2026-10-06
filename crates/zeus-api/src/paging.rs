//! Pagination: `limit` (default 50, maximum 100) and an opaque cursor; results are `{items, next_cursor}`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeus_types::limits::{DEFAULT_PAGE_SIZE, MAX_PAGE_SIZE};

/// Paging parameters shared by every list method (00 section 10).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct PageRequest {
    /// Page size. Absent: 50. Values above 100 are clamped to 100; 0 is invalid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
    /// Opaque cursor from a previous result. Valid only for the same method and filters.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
}

impl PageRequest {
    /// The effective page size.
    pub fn effective_limit(&self) -> usize {
        match self.limit {
            None => DEFAULT_PAGE_SIZE,
            Some(n) => (n as usize).clamp(1, MAX_PAGE_SIZE),
        }
    }

    /// `limit: 0` is a client bug and is rejected rather than silently changed.
    pub fn validate(&self) -> Result<(), crate::error::ApiError> {
        if self.limit == Some(0) {
            return Err(crate::error::ApiError::invalid_params(
                "limit must be at least 1",
            ));
        }
        if self.cursor.as_ref().is_some_and(|c| c.len() > 512) {
            return Err(crate::error::ApiError::invalid_params("cursor is too long"));
        }
        Ok(())
    }
}

/// One page of results.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Page<T> {
    /// The items.
    pub items: Vec<T>,
    /// Cursor for the next page, `null` at the end.
    pub next_cursor: Option<String>,
}

impl<T> Page<T> {
    /// A page with no further results.
    pub fn last(items: Vec<T>) -> Self {
        Self {
            items,
            next_cursor: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limits_default_clamp_and_reject_zero() {
        assert_eq!(PageRequest::default().effective_limit(), 50);
        assert_eq!(
            PageRequest {
                limit: Some(1000),
                cursor: None
            }
            .effective_limit(),
            100
        );
        assert_eq!(
            PageRequest {
                limit: Some(7),
                cursor: None
            }
            .effective_limit(),
            7
        );
        assert!(
            PageRequest {
                limit: Some(0),
                cursor: None
            }
            .validate()
            .is_err()
        );
        assert!(
            PageRequest {
                limit: Some(10),
                cursor: Some("x".repeat(513))
            }
            .validate()
            .is_err()
        );
    }
}
