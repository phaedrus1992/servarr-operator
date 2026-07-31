//! Test-only shared helpers for the tenant-safe sanitizer property tests.
//!
//! Kept in one module (rather than duplicated per test module) so the two
//! no-leak invariants — the seed token and the charset allowlist — are defined
//! once and asserted everywhere.

/// Fixed recognizable token used to seed sensitive content (API-server Status
/// messages, exec-command args, response bodies, URLs) in the no-leak property
/// tests. Distinctive enough that it can never coincide with a legitimate
/// sanitizer output.
pub(crate) const SEED_TOKEN: &str = "SEED-SECRET-TOKEN";

/// The charset every tenant-safe summary is permitted to contain. Mirrors the
/// brief's `^[A-Za-z0-9 ._()-]*$` invariant — a tenant-visible message can't
/// smuggle arbitrary content through — with one addition: `:`. The status
/// carriers legitimately emit `"status: {code}"` (fixed-format punctuation,
/// not smuggled content), so the bare brief regex over-rejects legitimate
/// output; `:` is the only character the sanitizers produce that the class
/// omits.
pub(crate) fn is_tenant_safe_charset(s: &str) -> bool {
    s.chars().all(|c| {
        matches!(
            c,
            'A'..='Z' | 'a'..='z' | '0'..='9' | ' ' | ':' | '.' | '_' | '(' | ')' | '-'
        )
    })
}
