//! Environment-variable reads that log why they fell back to a default.
//!
//! [`std::env::var`] fails in two different ways. [`VarError::NotPresent`] means the operator
//! was never given the variable. [`VarError::NotUnicode`] means the operator *was* given the
//! variable but cannot read it. Matching on a bare `Err(_)` collapses the two, so a typo that
//! produces a non-UTF-8 value looks exactly like a variable nobody set (#725, #726, #730).
//! These helpers keep the two apart.
//!
//! Each read comes in two forms. [`var`] and [`var_bool`] are lenient: an unreadable value is a
//! `warn!`, and the caller's default still applies. [`var_strict`], [`var_bool_strict`], and
//! [`var_path`] are strict: an unreadable value is an [`EnvError`], so the caller can refuse to
//! start (#732).
//!
//! Pick by one question: does the default change a security or availability posture? A default
//! that widens admission, disables a webhook, or serves a certificate nobody chose is not a safe
//! guess, so those call sites take the strict form. That judgement belongs at the call site, not
//! inside one shared helper.
//!
//! A log line or an error message names the variable and gives the length of a bad value. It
//! never contains the value itself, so these helpers stay safe for a variable that carries a
//! credential.

use std::env::VarError;
use std::path::PathBuf;

use tracing::{debug, warn};

/// A variable is set, but the operator cannot use its value.
///
/// [`var`] and [`var_bool`] fold this case into the caller's default. The strict siblings return
/// it instead, so a call site where a default changes a security or availability posture can
/// refuse to start rather than substitute a value nobody asked for (#732).
///
/// A message names the variable and gives the length of a bad value. It never contains the value
/// itself, so it stays safe to log for a variable that carries a credential.
#[derive(Debug, thiserror::Error)]
pub enum EnvError {
    /// The variable is set to a value that is not valid UTF-8.
    #[error(
        "{key} is set but is not valid UTF-8 ({bytes} bytes). \
         Set {key} to a UTF-8 value, or unset it to use the default."
    )]
    NotUnicode {
        /// The variable's name.
        key: String,
        /// The length of the unreadable value, in bytes.
        bytes: usize,
    },
    /// The variable is set to a value outside the accepted boolean spellings.
    #[error(
        "{key} is set to an unrecognized value ({bytes} bytes). \
         Set {key} to one of true/false/1/0/yes/no, or unset it to use the default."
    )]
    NotBool {
        /// The variable's name.
        key: String,
        /// The length of the unrecognized value, in bytes.
        bytes: usize,
    },
    /// The variable is set to a readable value that the caller cannot use.
    #[error(
        "{key} is set to a value this operator cannot use: {reason} \
         Correct {key}, or unset it to use the default."
    )]
    Unusable {
        /// The variable's name.
        key: String,
        /// What the caller expected.
        ///
        /// `&'static str` on purpose. A variable can carry a credential, and this message
        /// reaches the operator's logs, so the type refuses to hold anything read at run time.
        /// A reason that needs a measurement gets its own variant, as [`Self::NotBool`] does.
        reason: &'static str,
    },
}

impl EnvError {
    /// Builds an [`EnvError::Unusable`]. State in `reason` what the caller expected.
    pub(crate) fn unusable(key: &str, reason: &'static str) -> Self {
        Self::Unusable {
            key: key.to_string(),
            reason,
        }
    }
}

/// Reads `key`, returning `None` when the caller should use its own default.
///
/// Logs a `debug!` when the variable is not set and a `warn!` when it is set to a value that
/// is not valid UTF-8. Both cases return `None`, so the caller's default still applies — the
/// difference is only that the second case is now visible in the logs.
///
/// The log lines name the variable and give the length of a bad value. They never contain the
/// value itself, so this helper stays safe to use for a variable that carries a credential.
pub(crate) fn var(key: &str) -> Option<String> {
    match var_strict(key) {
        Ok(value) => value,
        Err(error) => {
            warn!(env_var = key, %error, "ignoring it and using the default");
            None
        }
    }
}

/// Reads `key`, keeping "not set" and "set but unreadable" apart.
///
/// This is the fallible sibling of [`var`]. It returns `Ok(None)` only when the variable is not
/// set, so a caller whose default changes a security or availability posture can refuse to start
/// on an unreadable value instead of substituting a value the user did not ask for.
///
/// Use [`var`] for a genuinely optional variable, where falling back to a default is a safe guess.
///
/// # Errors
///
/// Returns [`EnvError::NotUnicode`] when `key` is set to a value that is not valid UTF-8.
pub(crate) fn var_strict(key: &str) -> Result<Option<String>, EnvError> {
    match std::env::var(key) {
        Ok(value) => Ok(Some(value)),
        Err(VarError::NotPresent) => {
            debug!(env_var = key, "env var is not set, using the default");
            Ok(None)
        }
        Err(VarError::NotUnicode(raw)) => Err(EnvError::NotUnicode {
            key: key.to_string(),
            bytes: raw.len(),
        }),
    }
}

/// Reads `key` as a boolean, refusing a value it does not recognize.
///
/// Accepts `true`/`false`, `1`/`0`, and `yes`/`no`, each case-insensitively. An unset or empty
/// value yields `default`. Any other value is an error, because a user who writes `on` or `y`
/// expressed an intent and must not silently receive the opposite (#732).
///
/// Use [`var_bool`] for a genuinely optional flag, where the default is a safe guess.
///
/// # Errors
///
/// Returns [`EnvError::NotUnicode`] when `key` is not valid UTF-8. Returns [`EnvError::NotBool`]
/// when `key` holds a non-empty value outside the accepted spellings.
pub fn var_bool_strict(key: &str, default: bool) -> Result<bool, EnvError> {
    let Some(value) = var_strict(key)? else {
        return Ok(default);
    };
    if value.is_empty() {
        debug!(
            env_var = key,
            default, "env var is set but empty, treating it as not set"
        );
        return Ok(default);
    }
    if value.eq_ignore_ascii_case("true") || value == "1" || value.eq_ignore_ascii_case("yes") {
        return Ok(true);
    }
    if value.eq_ignore_ascii_case("false") || value == "0" || value.eq_ignore_ascii_case("no") {
        return Ok(false);
    }
    Err(EnvError::NotBool {
        key: key.to_string(),
        bytes: value.len(),
    })
}

/// Reads `key` as a filesystem path.
///
/// A path is `OsString`-shaped, so this never rejects a path that is not valid UTF-8. On Unix
/// such a path is a legal path, and reading it as an `OsString` makes it work rather than warn
/// and fall back (#733).
///
/// Returns `Ok(None)` only when the variable is not set. An empty value is an error: an empty
/// path names nothing, and the caller's default is not the path the operator was given.
///
/// # Errors
///
/// Returns [`EnvError::Unusable`] when `key` is set to an empty value.
pub(crate) fn var_path(key: &str) -> Result<Option<PathBuf>, EnvError> {
    match std::env::var_os(key) {
        None => {
            debug!(env_var = key, "env var is not set, using the default");
            Ok(None)
        }
        Some(raw) if raw.is_empty() => Err(EnvError::unusable(
            key,
            "expected a filesystem path, got an empty value.",
        )),
        Some(raw) => Ok(Some(PathBuf::from(raw))),
    }
}

/// Reads `key` as a boolean, returning `default` when it is unset or unusable.
///
/// Accepts `true`/`false`, `1`/`0`, and `yes`/`no`, each case-insensitively. An empty value
/// counts as unset. Any other value logs a `warn!` and yields `default`.
///
/// The log line gives the length of the value, not the value itself, for the reason given on
/// [`var`]. Use [`var_bool_strict`] where the default is not a safe guess.
pub(crate) fn var_bool(key: &str, default: bool) -> bool {
    match var_bool_strict(key, default) {
        Ok(value) => value,
        Err(error) => {
            warn!(env_var = key, %error, default, "using the default");
            default
        }
    }
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

    // ── var_strict ──

    #[test]
    fn var_strict_returns_the_value_when_set() {
        temp_env::with_var(KEY, Some("hello"), || {
            assert_eq!(var_strict(KEY).unwrap(), Some("hello".to_string()));
        });
    }

    #[test]
    fn var_strict_returns_none_only_when_not_set() {
        temp_env::with_var_unset(KEY, || {
            assert_eq!(var_strict(KEY).unwrap(), None);
        });
    }

    #[test]
    #[cfg(unix)]
    fn var_strict_errors_when_set_but_not_valid_utf8() {
        temp_env::with_var(KEY, Some(invalid_utf8()), || {
            let error = var_strict(KEY).expect_err("an unreadable value must not look unset");
            assert!(matches!(error, EnvError::NotUnicode { .. }));
            assert!(error.to_string().contains(KEY));
        });
    }

    #[test]
    #[cfg(unix)]
    fn var_strict_never_reports_the_bad_value_itself() {
        temp_env::with_var(KEY, Some(invalid_utf8()), || {
            let message = var_strict(KEY).expect_err("must error").to_string();
            assert!(
                !message.contains("fo"),
                "message leaked the value: {message}"
            );
        });
    }

    // ── var_bool_strict ──

    #[test]
    fn var_bool_strict_accepts_every_recognized_spelling() {
        for value in ["true", "TRUE", "1", "yes"] {
            temp_env::with_var(KEY, Some(value), || {
                assert!(var_bool_strict(KEY, false).unwrap(), "{value} is true");
            });
        }
        for value in ["false", "FALSE", "0", "no"] {
            temp_env::with_var(KEY, Some(value), || {
                assert!(!var_bool_strict(KEY, true).unwrap(), "{value} is false");
            });
        }
    }

    #[test]
    fn var_bool_strict_uses_the_default_when_unset_or_empty() {
        temp_env::with_var_unset(KEY, || {
            assert!(var_bool_strict(KEY, true).unwrap());
        });
        temp_env::with_var(KEY, Some(""), || {
            assert!(var_bool_strict(KEY, true).unwrap());
        });
    }

    #[test]
    fn var_bool_strict_errors_on_an_unrecognized_value() {
        for value in ["on", "enabled", "y", "maybe"] {
            temp_env::with_var(KEY, Some(value), || {
                let error = var_bool_strict(KEY, false)
                    .expect_err("an unrecognized value must not yield the default");
                assert!(error.to_string().contains(KEY), "{value}");
            });
        }
    }

    #[test]
    #[cfg(unix)]
    fn var_bool_strict_errors_when_not_valid_utf8() {
        temp_env::with_var(KEY, Some(invalid_utf8()), || {
            assert!(var_bool_strict(KEY, false).is_err());
        });
    }

    // ── var_path ──

    #[test]
    fn var_path_returns_the_path_when_set() {
        temp_env::with_var(KEY, Some("/etc/webhook/tls.crt"), || {
            assert_eq!(
                var_path(KEY).unwrap(),
                Some(std::path::PathBuf::from("/etc/webhook/tls.crt"))
            );
        });
    }

    #[test]
    fn var_path_returns_none_when_not_set() {
        temp_env::with_var_unset(KEY, || {
            assert_eq!(var_path(KEY).unwrap(), None);
        });
    }

    /// A path is `OsString`-shaped, so a non-UTF-8 path must work rather than warn and fall back.
    #[test]
    #[cfg(unix)]
    fn var_path_accepts_a_path_that_is_not_valid_utf8() {
        use std::os::unix::ffi::OsStrExt;
        temp_env::with_var(KEY, Some(invalid_utf8()), || {
            let path = var_path(KEY)
                .unwrap()
                .expect("a non-UTF-8 path is still a path");
            assert_eq!(path.as_os_str().as_bytes(), &[0x66, 0x80, 0x6f]);
        });
    }

    #[test]
    fn var_path_errors_on_an_empty_value() {
        temp_env::with_var(KEY, Some(""), || {
            let error = var_path(KEY).expect_err("an empty path names nothing");
            assert!(error.to_string().contains(KEY));
        });
    }

    const RECOGNIZED: [&str; 6] = ["true", "false", "1", "0", "yes", "no"];

    fn is_recognized(value: &str) -> bool {
        RECOGNIZED
            .iter()
            .any(|known| value.eq_ignore_ascii_case(known))
    }

    /// Rewrites `canonical` with the characters `uppercase_mask` selects put in upper case.
    fn respell(canonical: &str, uppercase_mask: &[bool]) -> String {
        canonical
            .chars()
            .enumerate()
            .map(|(i, c)| {
                if *uppercase_mask.get(i).unwrap_or(&false) {
                    c.to_ascii_uppercase()
                } else {
                    c
                }
            })
            .collect()
    }

    proptest::proptest! {
        /// The strict reader returns the value unchanged for every readable value.
        #[test]
        fn var_strict_round_trips_every_readable_value(value in "[^\u{0}]{0,64}") {
            temp_env::with_var(KEY, Some(value.as_str()), || {
                proptest::prop_assert_eq!(var_strict(KEY)?, Some(value.clone()));
                Ok(())
            })?;
        }

        /// The lenient reader agrees with the strict one wherever the strict one succeeds.
        /// They must never disagree on a value both can read.
        #[test]
        fn var_agrees_with_var_strict_on_every_readable_value(value in "[^\u{0}]{0,64}") {
            temp_env::with_var(KEY, Some(value.as_str()), || {
                proptest::prop_assert_eq!(var(KEY), var_strict(KEY)?);
                Ok(())
            })?;
        }

        /// The strict boolean reader never substitutes the default for a value it does not
        /// recognize. That is the whole difference from [`var_bool`], so it must hold for
        /// every unrecognized value, not just the four in the example test.
        #[test]
        fn var_bool_strict_errors_for_every_unrecognized_value(value in "[^\u{0}]{0,32}") {
            proptest::prop_assume!(!value.is_empty() && !is_recognized(&value));
            temp_env::with_var(KEY, Some(value.as_str()), || {
                proptest::prop_assert!(var_bool_strict(KEY, true).is_err());
                proptest::prop_assert!(var_bool_strict(KEY, false).is_err());
                Ok(())
            })?;
        }

        /// Case never changes the strict reader's result either.
        #[test]
        fn var_bool_strict_ignores_case_for_every_recognized_value(
            index in 0usize..RECOGNIZED.len(),
            uppercase_mask in proptest::collection::vec(proptest::bool::ANY, 5),
        ) {
            let canonical = RECOGNIZED[index];
            let spelled = respell(canonical, &uppercase_mask);
            let expected = matches!(canonical, "true" | "1" | "yes");
            temp_env::with_var(KEY, Some(spelled.as_str()), || {
                proptest::prop_assert_eq!(var_bool_strict(KEY, !expected)?, expected);
                Ok(())
            })?;
        }

        /// A non-empty value is always a usable path, whatever it contains. Only the empty
        /// value is refused, and the variable's name always reaches the message.
        #[test]
        fn var_path_accepts_every_non_empty_value(value in "[^\u{0}]{1,64}") {
            temp_env::with_var(KEY, Some(value.as_str()), || {
                let path = var_path(KEY)?.expect("a non-empty value is a path");
                proptest::prop_assert_eq!(path, std::path::PathBuf::from(&value));
                Ok(())
            })?;
        }

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
            let spelled = respell(canonical, &uppercase_mask);
            let expected = matches!(canonical, "true" | "1" | "yes");
            temp_env::with_var(KEY, Some(spelled.as_str()), || {
                proptest::prop_assert_eq!(var_bool(KEY, !expected), expected);
                Ok(())
            })?;
        }
    }
}
