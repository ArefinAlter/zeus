//! Local, authenticated A2A 1.0 JSON-RPC bridge. It has no approval authority.
#![forbid(unsafe_code)]

mod config;
mod rpc;
mod store;
mod transport;

pub use config::{Config, Peer};
pub use rpc::Bridge;
pub use transport::serve;

#[cfg(test)]
mod tests;
