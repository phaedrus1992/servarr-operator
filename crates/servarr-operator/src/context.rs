use kube::Client;
use kube::runtime::events::Reporter;
use servarr_crds::{AppType, ImageSpec};
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
    match std::env::var("WATCH_ALL_NAMESPACES") {
        Ok(v) if v.eq_ignore_ascii_case("true") || v == "1" || v.eq_ignore_ascii_case("yes") => {
            true
        }
        Ok(v)
            if v.eq_ignore_ascii_case("false")
                || v == "0"
                || v.eq_ignore_ascii_case("no")
                || v.is_empty() =>
        {
            false
        }
        Ok(v) => {
            warn!(
                value = %v,
                "unrecognized WATCH_ALL_NAMESPACES value, expected true/false/1/0/yes/no; defaulting to false"
            );
            false
        }
        Err(_) => false,
    }
}

impl Context {
    pub(crate) fn new(client: Client) -> Self {
        let (image_overrides, legacy_image_override_apps) = load_image_overrides();
        let reporter = Reporter {
            controller: "servarr-operator".into(),
            instance: std::env::var("POD_NAME").ok(),
        };
        let watch_all = watch_all_namespaces();
        let watch_namespace = if watch_all {
            None
        } else {
            std::env::var("WATCH_NAMESPACE")
                .ok()
                .filter(|s| !s.is_empty())
        };
        if let Some(ref ns) = watch_namespace {
            info!(%ns, "namespace-scoped mode");
        } else {
            info!("cluster-scoped mode (watching all namespaces)");
        }
        Self {
            client,
            image_overrides,
            legacy_image_override_apps,
            reporter,
            watch_namespace,
            app_api_base_override: None,
        }
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

        if let Ok(repo) = std::env::var(&repo_key) {
            let tag = std::env::var(&tag_key).unwrap_or_default();
            info!(%name, %repo, %tag, "loaded image override from env");
            overrides.insert(
                name.to_string(),
                ImageSpec {
                    repository: repo,
                    tag,
                    digest: String::new(),
                    pull_policy: "IfNotPresent".into(),
                },
            );
        }
    }

    // Issue #44: fall back to the pre-rename DEFAULT_IMAGE_OVERSEERR_* env vars if the new
    // DEFAULT_IMAGE_SEERR_* ones aren't set. Without this, a Helm release with a lingering
    // `defaultImages.overseerr` value override renders DEFAULT_IMAGE_OVERSEERR_REPO/TAG (the
    // chart template emits one env var pair per key in the user's values, regardless of
    // whether the operator still recognizes that key) — and that override would silently stop
    // applying after upgrade, falling back to the new default image with no warning.
    if !overrides.contains_key("seerr")
        && let Ok(repo) = std::env::var("DEFAULT_IMAGE_OVERSEERR_REPO")
    {
        let tag = std::env::var("DEFAULT_IMAGE_OVERSEERR_TAG").unwrap_or_default();
        warn!(
            %repo, %tag,
            "loaded image override from deprecated DEFAULT_IMAGE_OVERSEERR_* env vars — \
             rename defaultImages.overseerr to defaultImages.seerr in your Helm values"
        );
        overrides.insert(
            "seerr".to_string(),
            ImageSpec {
                repository: repo,
                tag,
                digest: String::new(),
                pull_policy: "IfNotPresent".into(),
            },
        );
        legacy.insert("seerr".to_string());
    }

    (overrides, legacy)
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

    /// Mirrors the watch_namespace derivation from Context::new.
    fn derive_watch_namespace() -> Option<String> {
        let watch_all = watch_all_namespaces();
        if watch_all {
            None
        } else {
            std::env::var("WATCH_NAMESPACE")
                .ok()
                .filter(|s| !s.is_empty())
        }
    }

    #[test]
    fn watch_namespace_returned_when_not_all() {
        temp_env::with_vars(
            [
                ("WATCH_ALL_NAMESPACES", Some("false")),
                ("WATCH_NAMESPACE", Some("my-ns")),
            ],
            || {
                assert_eq!(derive_watch_namespace(), Some("my-ns".to_string()));
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
                assert_eq!(derive_watch_namespace(), None);
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
                assert_eq!(derive_watch_namespace(), None);
            },
        );
    }

    #[test]
    fn watch_namespace_none_when_unset() {
        temp_env::with_vars(
            [
                ("WATCH_ALL_NAMESPACES", Some("false")),
                ("WATCH_NAMESPACE", None::<&str>),
            ],
            || {
                assert_eq!(derive_watch_namespace(), None);
            },
        );
    }
}
