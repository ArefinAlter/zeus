//! The native-messaging host: a thin, strict relay between the browser extension and `zeusd` (spec 02).
//!
//! The browser starts `zeus-native-host` when the extension calls `runtime.connectNative`. The host:
//!
//! 1. learns **which extension launched it** from `argv` ([`argv`]) and checks it against its own allow-list
//!    ([`allowlist`]) so a tampered browser manifest alone cannot widen access;
//! 2. speaks the native-messaging framing on stdio ([`frame`]): 4-byte native-endian length, UTF-8 JSON, at most
//!    4 MiB extension to host and 1,000,000 bytes host to extension;
//! 3. authenticates to the daemon as role `native_host` (daemon API handshake with an `attach` block) and relays
//!    frames as `nm.frame` notifications ([`relay`], not implemented yet).
//!
//! It never writes anything to stdout except frames, never logs frame bodies, never stores cookies.
//!
//! The same crate installs and removes the browser host manifests ([`install`]); `zeus native-host install`
//! calls it.

#![forbid(unsafe_code)]

pub mod allowlist;
pub mod argv;
pub mod error;
pub mod frame;
pub mod install;
pub mod relay;

pub use error::HostError;

/// The native-messaging host name registered with the browsers (spec 00 section 3).
pub const HOST_NAME: &str = "dev.zeus.native_host";
