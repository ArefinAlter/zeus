//! Retained-capability local copies and bounded comparisons shared by Zeus frontends.
//! Callers own grants, approval, quarantine and current-authority policy. This crate never widens it.
#![forbid(unsafe_code)]
pub mod compare;
pub mod copies;
pub mod failure;
pub mod paths;
fn random_id() -> String {
    let mut bytes = [0u8; 16];
    rand::fill(&mut bytes);
    hex::encode(bytes)
}
