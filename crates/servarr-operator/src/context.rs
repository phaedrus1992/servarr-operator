use kube::Client;
use kube::runtime::events::Reporter;
use servarr_crds::{AppDefaults, AppType, ImageSpec};

use crate::env::EnvError;
use std::collections::{HashMap, HashSet};
use tracing::{info, warn};

pub struct Context {
    pub client: Client,
    /// Image overrides loaded from DEFAULT_IMAGE_<APP>_REPO / DEFAULT_IMAGE_<APP>_TAG env vars.
    /// Keys are lowercase app names (e.g. "sonarr", "radarr").
    pub image_overrides: HashMap<String, ImageSpec>,
    /// Lowercase app names whose `image_overrides` entry came from a deprecated
    /// pre-rename env var fallback (e.g. `seerr` via `DEFAULT_IMAGE_OVERSEERR_*`) rather
    /// than an explicit override for the app's current name. Reconcile uses this to
    /// publish a Warning Event so a stale Helm value can't silently drive the image
    /// with only a startup log line to notice it (#534).
    pub legacy_image_override_apps: HashSet<String>,
    /// Reporter identity used when publishing Kubernetes Events.
    pub reporter: Reporter,
    /// The namespace to watch. When `Some`, the operator uses `Api::namespaced()`
    /// and only needs `Role`/`RoleBinding` privileges. When `None`, the operator
    /// watches all namespaces and requires `ClusterRole`/`ClusterRoleBinding`.
    ///
    /// Defaults to the pod's own namespace (from `WATCH_NAMESPACE` env, typically
    /// set via the downward API). Set `WATCH_ALL_NAMESPACES=true` to opt into
    /// cluster-scoped mode.
    pub watch_namespace: Option<String>,
    /// Override base URL for in-cluster app API calls. `None` in production (URLs
    /// are built from `<name>.<ns>.svc:<port>`). Tests set this to a wiremock URI.
    pub app_api_base_override: Option<String>,
}

/// Parses `WATCH_ALL_NAMESPACES`. Standalone (not just an inline step of [`Context::new`]) so
/// callers that need the cluster-scoped/namespace-scoped decision before a full `Context`
/// exists — e.g. deciding whether the `crd_check` self-check has the RBAC to run (#543,
/// `ClusterRole`-only) — don't duplicate the parsing.
pub fn watch_all_namespaces() -> bool {
    crate::env::var_bool("WATCH_ALL_NAMESPACES", false)
}

/// Resolves the namespace to watch. `Ok(None)` means cluster-scoped.
///
/// An empty `WATCH_NAMESPACE` widens the operator from one namespace to the whole cluster, so
/// it warns rather than falling through in silence. The downward API never produces an empty
/// value, but a hand-written pod spec or a `valueFrom` on a missing key does.
///
/// An *unreadable* value widens the same way, and it is not a case the user chose. "Unset, so
/// watch everything" and "set, so it must be usable" are different situations, and only the
/// first is an instruction. Widening scope changes a security posture, so the second is fatal.
///
/// # Errors
///
/// Returns [`EnvError`] when `WATCH_NAMESPACE` is set to a value that is not valid UTF-8.
pub fn watch_namespace() -> Result<Option<String>, EnvError> {
    if watch_all_namespaces() {
        return Ok(None);
    }
    let Some(ns) = crate::env::var_strict("WATCH_NAMESPACE")? else {
        return Ok(None);
    };
    if ns.is_empty() {
        warn!(
            "WATCH_NAMESPACE is set but empty, falling back to cluster-scoped mode; \
             set it to a namespace name or unset it to make this deliberate"
        );
        return Ok(None);
    }
    Ok(Some(ns))
}

impl Context {
    /// # Errors
    ///
    /// Returns [`EnvError`] when `WATCH_NAMESPACE` is set to a value the operator cannot read.
    /// Widening to cluster scope on a value the user did not choose is not a safe default.
    pub(crate) fn new(client: Client) -> Result<Self, EnvError> {
        let (image_overrides, legacy_image_override_apps) = load_image_overrides();
        let reporter = Reporter {
            controller: "servarr-operator".into(),
            instance: crate::env::var("POD_NAME"),
        };
        let watch_namespace = watch_namespace()?;
        if let Some(ref ns) = watch_namespace {
            info!(%ns, "namespace-scoped mode");
        } else {
            info!("cluster-scoped mode (watching all namespaces)");
        }
        Ok(Self {
            client,
            image_overrides,
            legacy_image_override_apps,
            reporter,
            watch_namespace,
            app_api_base_override: None,
        })
    }
}

/// Read DEFAULT_IMAGE_<APP>_REPO and DEFAULT_IMAGE_<APP>_TAG env vars for each known app.
///
/// Driven by [`AppType::ALL`] so the set of apps honored here can never drift
/// from the enum (the Helm chart emits one env-var pair per `defaultImages`
/// key, and this must cover every one of them or the override is silently
/// dropped).
///
/// Returns the overrides plus the subset of app keys whose entry came from a deprecated
/// pre-rename fallback env var rather than an override for the app's current name.
fn load_image_overrides() -> (HashMap<String, ImageSpec>, HashSet<String>) {
    let mut overrides = HashMap::new();
    let mut legacy = HashSet::new();

    for app in AppType::ALL {
        let name = app.as_str();
        let repo_key = format!("DEFAULT_IMAGE_{}_REPO", name.to_uppercase());
        let tag_key = format!("DEFAULT_IMAGE_{}_TAG", name.to_uppercase());

        let Some(repo) = read_override_repo(&repo_key, name) else {
            continue;
        };
        let Some(tag) = read_override_tag(&tag_key, name) else {
            continue;
        };
        let spec = ImageSpec {
            repository: repo,
            tag,
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        };
        info!(%name, image = %effective_image(app, &spec), "loaded image override from env");
        overrides.insert(name.to_string(), spec);
    }

    // Issue #44: fall back to the pre-rename DEFAULT_IMAGE_OVERSEERR_* env vars if the new
    // DEFAULT_IMAGE_SEERR_* ones aren't set. Without this, a Helm release with a lingering
    // `defaultImages.overseerr` value override renders DEFAULT_IMAGE_OVERSEERR_REPO/TAG (the
    // chart template emits one env var pair per key in the user's values, regardless of
    // whether the operator still recognizes that key) — and that override would silently stop
    // applying after upgrade, falling back to the new default image with no warning.
    if !overrides.contains_key("seerr")
        && let Some(repo) = read_override_repo("DEFAULT_IMAGE_OVERSEERR_REPO", "seerr")
        && let Some(tag) = read_override_tag("DEFAULT_IMAGE_OVERSEERR_TAG", "seerr")
    {
        let spec = ImageSpec {
            repository: repo,
            tag,
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        };
        warn!(
            image = %effective_image(&AppType::Seerr, &spec),
            "loaded image override from deprecated DEFAULT_IMAGE_OVERSEERR_* env vars — \
             rename defaultImages.overseerr to defaultImages.seerr in your Helm values"
        );
        overrides.insert("seerr".to_string(), spec);
        legacy.insert("seerr".to_string());
    }

    (overrides, legacy)
}

/// Reads one `DEFAULT_IMAGE_<APP>_TAG`, returning `None` when the whole override must be dropped.
///
/// An *unset* tag yields `Some("")`. That is deliberate: the chart renders `value: ""` when a
/// user overrides only `repository`, and the compiled default tag fills the empty half in during
/// the merge (#38).
///
/// A *present but unreadable* tag yields `None`. Keeping the override there would pair the user's
/// repository with the operator's compiled default tag — an image nobody requested, which may not
/// exist in the registry, or may exist and be the wrong build (#734). The consistent compiled
/// default is the better answer.
fn read_override_tag(tag_key: &str, name: &str) -> Option<String> {
    match crate::env::var_strict(tag_key) {
        Ok(tag) => Some(tag.unwrap_or_default()),
        Err(error) => {
            warn_dropped_override(name, &error);
            None
        }
    }
}

/// Reads one `DEFAULT_IMAGE_<APP>_REPO`, returning `None` when there is no override to apply.
///
/// An *unset* repository means the user asked for no override on this app, so there is nothing to
/// report. An *empty* one is kept, for the same reason an empty tag is: the chart renders
/// `value: ""` when a user overrides only the other half.
///
/// A *present but unreadable* repository drops the override, for the reason given on
/// [`read_override_tag`].
fn read_override_repo(repo_key: &str, name: &str) -> Option<String> {
    match crate::env::var_strict(repo_key) {
        Ok(repo) => repo,
        Err(error) => {
            warn_dropped_override(name, &error);
            None
        }
    }
}

/// Reports that an app's whole image override was dropped, and what the operator will pull now.
///
/// [`crate::env::var`]'s own warning says "using the default", which is too narrow here. The
/// operator is not defaulting one variable. It is discarding the app's whole override — including
/// a readable sibling variable the user did set — and will pull its own compiled image instead.
fn warn_dropped_override(name: &str, error: &crate::env::EnvError) {
    warn!(
        %name, %error,
        "dropping this app's whole image override; the operator will pull its own compiled \
         default image, not the requested one"
    );
}

/// Formats the image an override will actually produce, after the compiled defaults fill in the
/// fields the user left empty.
///
/// The raw override alone misleads a reader: an override that sets only `repository` prints an
/// empty tag, so nobody can tell from the startup logs which image the operator will pull (#734).
fn effective_image(app: &AppType, spec: &ImageSpec) -> String {
    let merged = match AppDefaults::try_for_app(app) {
        Ok(defaults) => spec.clone().merge_with(&defaults.image),
        // `AppDefaults::validate_all` reports a broken `image-defaults.toml` at startup, so this
        // process is already on its way down. Say the merge was skipped, so a reader does not
        // take the unmerged empty half for a real one.
        Err(error) => {
            warn!(%error, "cannot merge the compiled defaults; reporting the override as given");
            spec.clone()
        }
    };
    format!("{}:{}", merged.repository, merged.tag)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── load_image_overrides ──

    #[test]
    fn load_image_overrides_picks_up_repo_and_tag() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_SONARR_REPO", Some("ghcr.io/custom/sonarr")),
                ("DEFAULT_IMAGE_SONARR_TAG", Some("4.0")),
            ],
            || {
                let (overrides, _legacy) = load_image_overrides();
                let spec = overrides.get("sonarr").expect("sonarr override missing");
                assert_eq!(spec.repository, "ghcr.io/custom/sonarr");
                assert_eq!(spec.tag, "4.0");
                assert_eq!(spec.pull_policy, "IfNotPresent");
                assert!(spec.digest.is_empty());
            },
        );
    }

    #[test]
    fn load_image_overrides_tag_defaults_to_empty() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_RADARR_REPO", Some("my-repo/radarr")),
                ("DEFAULT_IMAGE_RADARR_TAG", None::<&str>),
            ],
            || {
                let (overrides, _legacy) = load_image_overrides();
                let spec = overrides.get("radarr").expect("radarr override missing");
                assert_eq!(spec.repository, "my-repo/radarr");
                assert!(spec.tag.is_empty());
            },
        );
    }

    /// Builds an `OsString` that is not valid UTF-8, to exercise the unreadable-value branch.
    #[cfg(unix)]
    fn invalid_utf8() -> std::ffi::OsString {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(vec![0x34, 0x80, 0x30])
    }

    /// #734: a present-but-unreadable tag must not pair the user's repository with the
    /// operator's compiled default tag. That names an image the user never requested, and it may
    /// not exist in the registry at all.
    #[test]
    #[cfg(unix)]
    fn load_image_overrides_drops_the_whole_override_on_an_unreadable_tag() {
        temp_env::with_var("DEFAULT_IMAGE_SONARR_REPO", Some("myrepo/sonarr"), || {
            temp_env::with_var("DEFAULT_IMAGE_SONARR_TAG", Some(invalid_utf8()), || {
                let (overrides, _legacy) = load_image_overrides();
                assert!(
                    !overrides.contains_key("sonarr"),
                    "an unreadable tag must not yield a hybrid repo/tag pairing"
                );
            });
        });
    }

    /// The repository half gets the same treatment as the tag half. Keeping a readable tag
    /// beside the operator's own repository would name an image nobody requested.
    #[test]
    #[cfg(unix)]
    fn load_image_overrides_drops_the_whole_override_on_an_unreadable_repo() {
        temp_env::with_var("DEFAULT_IMAGE_SONARR_TAG", Some("4.0"), || {
            temp_env::with_var("DEFAULT_IMAGE_SONARR_REPO", Some(invalid_utf8()), || {
                let (overrides, _legacy) = load_image_overrides();
                assert!(
                    !overrides.contains_key("sonarr"),
                    "an unreadable repository must not keep the readable tag beside a default repo"
                );
            });
        });
    }

    /// #38: an *unset* tag is deliberate. The chart renders `value: ""` when a user overrides
    /// only `repository`, and the compiled default tag fills it in during the merge.
    #[test]
    fn load_image_overrides_keeps_the_override_when_the_tag_is_merely_unset() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_SONARR_REPO", Some("myrepo/sonarr")),
                ("DEFAULT_IMAGE_SONARR_TAG", None::<&str>),
            ],
            || {
                let (overrides, _legacy) = load_image_overrides();
                let spec = overrides
                    .get("sonarr")
                    .expect("an unset tag keeps the override");
                assert_eq!(spec.repository, "myrepo/sonarr");
                assert!(spec.tag.is_empty());
            },
        );
    }

    /// The same rule applies to the deprecated pre-rename fallback.
    #[test]
    #[cfg(unix)]
    fn load_image_overrides_drops_the_legacy_override_on_an_unreadable_tag() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_SEERR_REPO", None::<&str>),
                ("DEFAULT_IMAGE_SEERR_TAG", None::<&str>),
                ("DEFAULT_IMAGE_OVERSEERR_REPO", Some("myrepo/overseerr")),
            ],
            || {
                temp_env::with_var("DEFAULT_IMAGE_OVERSEERR_TAG", Some(invalid_utf8()), || {
                    let (overrides, legacy) = load_image_overrides();
                    assert!(!overrides.contains_key("seerr"));
                    assert!(!legacy.contains("seerr"));
                });
            },
        );
    }

    /// #734: the log must name the image that will actually be pulled, so a reader can confirm
    /// an override took effect. The raw override alone prints an empty tag.
    #[test]
    fn effective_image_fills_an_empty_tag_from_the_compiled_default() {
        let spec = ImageSpec {
            repository: "myrepo/sonarr".into(),
            tag: String::new(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        };
        let rendered = effective_image(&AppType::Sonarr, &spec);
        assert!(rendered.starts_with("myrepo/sonarr:"), "{rendered}");
        assert!(
            !rendered.ends_with(':'),
            "the tag must come from the compiled default, got {rendered}"
        );
    }

    fn arb_image_spec() -> impl proptest::strategy::Strategy<Value = ImageSpec> {
        use proptest::prelude::*;
        ("[a-z0-9/._-]{0,24}", "[a-zA-Z0-9._-]{0,16}").prop_map(|(repository, tag)| ImageSpec {
            repository,
            tag,
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        })
    }

    proptest::proptest! {
        /// Every app in `AppType::ALL` has a compiled default with both halves filled, so the
        /// merge can never leave a half empty. A rendered `repo:` or `:tag` is a broken image
        /// reference, and the log exists to let a reader confirm which image will be pulled.
        #[test]
        fn effective_image_never_renders_an_empty_half(
            index in 0usize..AppType::ALL.len(),
            spec in arb_image_spec(),
        ) {
            let app = &AppType::ALL[index];
            let rendered = effective_image(app, &spec);
            proptest::prop_assert!(!rendered.starts_with(':'), "{0}", rendered);
            proptest::prop_assert!(!rendered.ends_with(':'), "{0}", rendered);
        }

        /// A half the user set explicitly always survives the merge. The compiled default fills
        /// the empty halves only — it never overwrites a value the user chose.
        #[test]
        fn effective_image_keeps_every_half_the_user_set(
            index in 0usize..AppType::ALL.len(),
            spec in arb_image_spec(),
        ) {
            let app = &AppType::ALL[index];
            let rendered = effective_image(app, &spec);
            let repo_prefix = format!("{}:", spec.repository);
            let tag_suffix = format!(":{}", spec.tag);
            if !spec.repository.is_empty() {
                proptest::prop_assert!(rendered.starts_with(&repo_prefix), "{0}", rendered);
            }
            if !spec.tag.is_empty() {
                proptest::prop_assert!(rendered.ends_with(&tag_suffix), "{0}", rendered);
            }
        }
    }

    #[test]
    fn effective_image_keeps_an_explicit_tag() {
        let spec = ImageSpec {
            repository: "myrepo/sonarr".into(),
            tag: "4.0".into(),
            digest: String::new(),
            pull_policy: "IfNotPresent".into(),
        };
        assert_eq!(
            effective_image(&AppType::Sonarr, &spec),
            "myrepo/sonarr:4.0"
        );
    }

    #[test]
    fn load_image_overrides_absent_repo_means_no_entry() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_LIDARR_REPO", None::<&str>),
                ("DEFAULT_IMAGE_LIDARR_TAG", None::<&str>),
            ],
            || {
                let (overrides, _legacy) = load_image_overrides();
                assert!(!overrides.contains_key("lidarr"));
            },
        );
    }

    #[test]
    fn load_image_overrides_multiple_apps() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_PROWLARR_REPO", Some("repo/prowlarr")),
                ("DEFAULT_IMAGE_PROWLARR_TAG", Some("latest")),
                ("DEFAULT_IMAGE_SABNZBD_REPO", Some("repo/sabnzbd")),
                ("DEFAULT_IMAGE_SABNZBD_TAG", Some("3.7")),
            ],
            || {
                let (overrides, _legacy) = load_image_overrides();
                assert!(overrides.contains_key("prowlarr"));
                assert!(overrides.contains_key("sabnzbd"));
                assert_eq!(overrides.get("prowlarr").unwrap().tag, "latest");
                assert_eq!(overrides.get("sabnzbd").unwrap().tag, "3.7");
            },
        );
    }

    #[test]
    fn load_image_overrides_honors_every_app_type() {
        // Regression: the app set was once a hardcoded 10-entry list while the
        // Helm chart emits a DEFAULT_IMAGE_* pair for every `defaultImages` key
        // (all 15 AppTypes). Apps added later — bazarr, subgen, jellyfin, plex,
        // ssh-bastion — had their overrides silently dropped. Driven by
        // AppType::ALL now, so this covers all of them.
        let mut env = vec![];
        for app in AppType::ALL {
            env.push((
                format!("DEFAULT_IMAGE_{}_REPO", app.as_str().to_uppercase()),
                Some(format!("registry.internal/{}", app.as_str())),
            ));
            env.push((
                format!("DEFAULT_IMAGE_{}_TAG", app.as_str().to_uppercase()),
                None,
            ));
        }
        temp_env::with_vars(env, || {
            let (overrides, _legacy) = load_image_overrides();
            assert_eq!(
                overrides.len(),
                AppType::ALL.len(),
                "every AppType must be overrideable"
            );
            for app in AppType::ALL {
                let name = app.as_str();
                let spec = overrides.get(name).unwrap_or_else(|| {
                    panic!("override missing for {name}");
                });
                assert_eq!(spec.repository, format!("registry.internal/{name}"));
            }
        });
    }

    #[test]
    fn load_image_overrides_falls_back_to_legacy_overseerr_env_vars() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_SEERR_REPO", None::<&str>),
                ("DEFAULT_IMAGE_SEERR_TAG", None::<&str>),
                (
                    "DEFAULT_IMAGE_OVERSEERR_REPO",
                    Some("registry.internal/mirror/overseerr"),
                ),
                ("DEFAULT_IMAGE_OVERSEERR_TAG", Some("1.35.0")),
            ],
            || {
                let (overrides, legacy) = load_image_overrides();
                let spec = overrides.get("seerr").expect("seerr override missing");
                assert_eq!(spec.repository, "registry.internal/mirror/overseerr");
                assert_eq!(spec.tag, "1.35.0");
                assert!(
                    legacy.contains("seerr"),
                    "seerr must be flagged as using the deprecated overseerr fallback so \
                     reconcile can surface a Warning Event (#534)"
                );
            },
        );
    }

    #[test]
    fn load_image_overrides_prefers_seerr_env_vars_over_legacy_overseerr_ones() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_SEERR_REPO", Some("ghcr.io/seerr-team/seerr")),
                ("DEFAULT_IMAGE_SEERR_TAG", Some("v3.4.1")),
                (
                    "DEFAULT_IMAGE_OVERSEERR_REPO",
                    Some("registry.internal/mirror/overseerr"),
                ),
                ("DEFAULT_IMAGE_OVERSEERR_TAG", Some("1.35.0")),
            ],
            || {
                let (overrides, legacy) = load_image_overrides();
                let spec = overrides.get("seerr").expect("seerr override missing");
                assert_eq!(spec.repository, "ghcr.io/seerr-team/seerr");
                assert_eq!(spec.tag, "v3.4.1");
                assert!(
                    !legacy.contains("seerr"),
                    "an explicit DEFAULT_IMAGE_SEERR_* override must not be flagged as legacy"
                );
            },
        );
    }

    #[test]
    fn load_image_overrides_ignores_unknown_app_env_vars() {
        temp_env::with_vars(
            [
                ("DEFAULT_IMAGE_NOTANAPP_REPO", Some("repo/notanapp")),
                ("DEFAULT_IMAGE_NOTANAPP_TAG", Some("1.0")),
            ],
            || {
                let (overrides, _legacy) = load_image_overrides();
                assert!(!overrides.contains_key("notanapp"));
            },
        );
    }

    // ── WATCH_ALL_NAMESPACES parsing ──

    #[test]
    fn watch_all_true_lowercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("true"), || {
            assert!(watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_true_uppercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("TRUE"), || {
            assert!(watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_one() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("1"), || {
            assert!(watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_yes() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("yes"), || {
            assert!(watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_false_lowercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("false"), || {
            assert!(!watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_false_uppercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("FALSE"), || {
            assert!(!watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_zero() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("0"), || {
            assert!(!watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_no() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("no"), || {
            assert!(!watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_empty_string() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some(""), || {
            assert!(!watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_unset() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", None::<&str>, || {
            assert!(!watch_all_namespaces());
        });
    }

    #[test]
    fn watch_all_unrecognized_defaults_false() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("maybe"), || {
            assert!(!watch_all_namespaces());
        });
    }

    // ── WATCH_NAMESPACE reading ──

    use super::watch_namespace as derive_watch_namespace;

    #[test]
    fn watch_namespace_returned_when_not_all() {
        temp_env::with_vars(
            [
                ("WATCH_ALL_NAMESPACES", Some("false")),
                ("WATCH_NAMESPACE", Some("my-ns")),
            ],
            || {
                assert_eq!(derive_watch_namespace().unwrap(), Some("my-ns".to_string()));
            },
        );
    }

    #[test]
    fn watch_namespace_none_when_watch_all_true() {
        temp_env::with_vars(
            [
                ("WATCH_ALL_NAMESPACES", Some("true")),
                ("WATCH_NAMESPACE", Some("my-ns")),
            ],
            || {
                assert_eq!(derive_watch_namespace().unwrap(), None);
            },
        );
    }

    #[test]
    fn watch_namespace_none_when_empty() {
        temp_env::with_vars(
            [
                ("WATCH_ALL_NAMESPACES", Some("false")),
                ("WATCH_NAMESPACE", Some("")),
            ],
            || {
                assert_eq!(derive_watch_namespace().unwrap(), None);
            },
        );
    }

    /// An unreadable value widens the operator to the whole cluster, and the user did not choose
    /// that. Refuse to start rather than take a broader scope than the one that was requested.
    #[test]
    #[cfg(unix)]
    fn watch_namespace_errors_when_set_but_unreadable() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("false"), || {
            temp_env::with_var("WATCH_NAMESPACE", Some(invalid_utf8()), || {
                let error = derive_watch_namespace()
                    .expect_err("an unreadable namespace must not widen the scope");
                assert!(error.to_string().contains("WATCH_NAMESPACE"));
            });
        });
    }

    #[test]
    fn watch_namespace_none_when_unset() {
        temp_env::with_vars(
            [
                ("WATCH_ALL_NAMESPACES", Some("false")),
                ("WATCH_NAMESPACE", None::<&str>),
            ],
            || {
                assert_eq!(derive_watch_namespace().unwrap(), None);
            },
        );
    }
}
