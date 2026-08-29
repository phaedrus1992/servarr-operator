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
///
/// The log lines name the variable and give the length of a bad value. They never contain the
/// value itself, so this helper stays safe to use for a variable that carries a credential.
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
                bytes = raw.len(),
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
///
/// The unrecognized-value log line gives the length of the value, not the value itself, for the
/// reason given on [`var`].
pub fn var_bool(key: &str, default: bool) -> bool {
    let Some(value) = var(key) else {
        return default;
    };
    if value.is_empty() {
        debug!(
            env_var = key,
            default, "env var is set but empty, treating it as not set"
        );
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
        bytes = value.len(),
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

    const RECOGNIZED: [&str; 6] = ["true", "false", "1", "0", "yes", "no"];

    fn is_recognized(value: &str) -> bool {
        RECOGNIZED
            .iter()
            .any(|known| value.eq_ignore_ascii_case(known))
    }

    proptest::proptest! {
        /// Any value outside the recognized set yields the caller's default, whatever it is.
        #[test]
        fn var_bool_yields_the_default_for_every_unrecognized_value(
            value in "[^\u{0}]{0,32}"
        ) {
            proptest::prop_assume!(!value.is_empty() && !is_recognized(&value));
            temp_env::with_var(KEY, Some(value.as_str()), || {
                proptest::prop_assert!(var_bool(KEY, true));
                proptest::prop_assert!(!var_bool(KEY, false));
                Ok(())
            })?;
        }

        /// Case never changes the result for a recognized value.
        #[test]
        fn var_bool_ignores_case_for_every_recognized_value(
            index in 0usize..RECOGNIZED.len(),
            uppercase_mask in proptest::collection::vec(proptest::bool::ANY, 5),
        ) {
            let canonical = RECOGNIZED[index];
            let spelled: String = canonical
                .chars()
                .enumerate()
                .map(|(i, c)| {
                    if *uppercase_mask.get(i).unwrap_or(&false) {
                        c.to_ascii_uppercase()
                    } else {
                        c
                    }
                })
                .collect();
            let expected = matches!(canonical, "true" | "1" | "yes");
            temp_env::with_var(KEY, Some(spelled.as_str()), || {
                proptest::prop_assert_eq!(var_bool(KEY, !expected), expected);
                Ok(())
            })?;
        }
    }
}
