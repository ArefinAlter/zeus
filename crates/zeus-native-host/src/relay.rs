//! The relay loop: native-messaging frames on stdio to `nm.frame` notifications on the daemon connection and back.
//!
//! Not implemented in the skeleton. The loop must (spec 02 sections 3.3 to 3.5 and 13.2):
//!
//! * read frames from stdin with [`crate::frame::read_frame`] (limit 4 MiB), parse with
//!   [`crate::frame::parse_payload`], and forward each as `nm.frame` over an authenticated daemon connection
//!   (role `native_host`, `attach` block from [`crate::argv::Launch`]);
//! * write daemon frames to stdout with [`crate::frame::write_frame`] (limit 1,000,000 bytes), answering the daemon
//!   with `nm.frame_too_large` instead of sending an oversized frame;
//! * close the connection on a zero or oversized length, on EOF in a frame, and after three consecutive invalid
//!   JSON payloads; exit when stdin reaches EOF or the daemon closes;
//! * write nothing else to stdout, ever (binary-mode stdio on Windows), and log only type, id, size and result.

use crate::argv::Launch;
use crate::error::HostError;

/// Run the relay for `launch` until the extension or the daemon closes the connection.
pub async fn run(_launch: &Launch) -> Result<(), HostError> {
    Err(HostError::NotImplemented(
        "relay loop (nm.frame between stdio and the daemon connection)",
    ))
}
