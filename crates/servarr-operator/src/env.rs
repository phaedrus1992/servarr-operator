//! Environment-variable reads that log why they fell back to a default.
//!
//! [`std::env::var`] fails in two different ways. [`VarError::NotPresent`] means the operator
//! was never given the variable. [`VarError::NotUnicode`] means the operator *was* given the
//! variable but cannot read it. Matching on a bare `Err(_)` collapses the two, so a typo that
//! produces a non-UTF-8 value looks exactly like a variable nobody set (#725, #726, #730).
//! These helpers keep the two apart: a missing variable is a `debug!`, an unreadable one is a
//! `warn!`.

use std::env::VarError;

use tracing::{debug, warn};

/// Reads `key`, returning `None` when the caller should use its own default.
///
/// Logs a `debug!` when the variable is not set and a `warn!` when it is set to a value that
/// is not valid UTF-8. Both cases return `None`, so the caller's default still applies — the
/// difference is only that the second case is now visible in the logs.
pub fn var(key: &str) -> Option<String> {
    match std::env::var(key) {
        Ok(value) => Some(value),
        Err(VarError::NotPresent) => {
            debug!(env_var = key, "env var is not set, using the default");
            None
        }
        Err(VarError::NotUnicode(raw)) => {
            warn!(
                env_var = key,
                value = %raw.to_string_lossy(),
                "env var is not valid UTF-8, ignoring it and using the default"
            );
            None
        }
    }
}

/// Reads `key` as a boolean, returning `default` when it is unset or unusable.
///
/// Accepts `true`/`false`, `1`/`0`, and `yes`/`no`, each case-insensitively. An empty value
/// counts as unset. Any other value logs a `warn!` and yields `default`.
pub fn var_bool(key: &str, default: bool) -> bool {
    let Some(value) = var(key) else {
        return default;
    };
    if value.is_empty() {
        return default;
    }
    if value.eq_ignore_ascii_case("true") || value == "1" || value.eq_ignore_ascii_case("yes") {
        return true;
    }
    if value.eq_ignore_ascii_case("false") || value == "0" || value.eq_ignore_ascii_case("no") {
        return false;
    }
    warn!(
        env_var = key,
        value = %value,
        default,
        "unrecognized value, expected true/false/1/0/yes/no; using the default"
    );
    default
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "SERVARR_TEST_ENV_HELPER";

    /// Builds an `OsString` that is not valid UTF-8, to exercise [`VarError::NotUnicode`].
    #[cfg(unix)]
    fn invalid_utf8() -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0x66, 0x80, 0x6f])
    }

    #[test]
    fn var_returns_the_value_when_set() {
        temp_env::with_var(KEY, Some("hello"), || {
            assert_eq!(var(KEY), Some("hello".to_string()));
        });
    }

    #[test]
    fn var_returns_none_when_not_set() {
        temp_env::with_var_unset(KEY, || {
            assert_eq!(var(KEY), None);
        });
    }

    #[test]
    #[cfg(unix)]
    fn var_returns_none_when_not_valid_utf8() {
        temp_env::with_var(KEY, Some(invalid_utf8()), || {
            assert_eq!(var(KEY), None);
        });
    }

    #[test]
    fn var_bool_accepts_every_truthy_spelling() {
        for value in ["true", "TRUE", "True", "1", "yes", "YES"] {
            temp_env::with_var(KEY, Some(value), || {
                assert!(var_bool(KEY, false), "{value} should parse as true");
            });
        }
    }

    #[test]
    fn var_bool_accepts_every_falsey_spelling() {
        for value in ["false", "FALSE", "False", "0", "no", "NO"] {
            temp_env::with_var(KEY, Some(value), || {
                assert!(!var_bool(KEY, true), "{value} should parse as false");
            });
        }
    }

    #[test]
    fn var_bool_uses_the_default_when_not_set() {
        temp_env::with_var_unset(KEY, || {
            assert!(var_bool(KEY, true));
            assert!(!var_bool(KEY, false));
        });
    }

    #[test]
    fn var_bool_uses_the_default_when_empty() {
        temp_env::with_var(KEY, Some(""), || {
            assert!(var_bool(KEY, true));
            assert!(!var_bool(KEY, false));
        });
    }

    #[test]
    fn var_bool_uses_the_default_when_unrecognized() {
        temp_env::with_var(KEY, Some("maybe"), || {
            assert!(var_bool(KEY, true));
            assert!(!var_bool(KEY, false));
        });
    }

    #[test]
    #[cfg(unix)]
    fn var_bool_uses_the_default_when_not_valid_utf8() {
        temp_env::with_var(KEY, Some(invalid_utf8()), || {
            assert!(var_bool(KEY, true));
            assert!(!var_bool(KEY, false));
        });
    }
}
