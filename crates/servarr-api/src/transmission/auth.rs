//! Basic-auth header encoding for the Transmission RPC client (#505).

use base64::Engine;

use super::BasicCredentials;

/// Build the HTTP `Authorization: Basic` header value for `creds`.
///
/// Base64-encodes `username:password`. Extracted from `TransmissionClient::new`
/// (#505) so the credential encoding is independently testable; the resulting
/// header value is marked `set_sensitive(true)` at the call site.
pub(super) fn basic_auth_header(creds: &BasicCredentials) -> String {
    format!(
        "Basic {}",
        base64_encode(&format!("{}:{}", creds.username(), creds.password()))
    )
}

/// Standard (padded) base64 of `input`. Uses the `base64` crate, already pinned for this
/// workspace, rather than a hand-rolled encoder.
pub(super) fn base64_encode(input: &str) -> String {
    base64::engine::general_purpose::STANDARD.encode(input)
}
