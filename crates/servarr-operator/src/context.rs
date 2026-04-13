use kube::Client;
use kube::runtime::events::Reporter;
use servarr_crds::ImageSpec;
use std::collections::HashMap;
use tracing::{info, warn};

pub struct Context {
    pub client: Client,
    /// Image overrides loaded from DEFAULT_IMAGE_<APP>_REPO / DEFAULT_IMAGE_<APP>_TAG env vars.
    /// Keys are lowercase app names (e.g. "sonarr", "radarr").
    pub image_overrides: HashMap<String, ImageSpec>,
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
}

impl Context {
    pub fn new(client: Client) -> Self {
        let image_overrides = load_image_overrides();
        let reporter = Reporter {
            controller: "servarr-operator".into(),
            instance: std::env::var("POD_NAME").ok(),
        };
        let watch_all = match std::env::var("WATCH_ALL_NAMESPACES") {
            Ok(v)
                if v.eq_ignore_ascii_case("true") || v == "1" || v.eq_ignore_ascii_case("yes") =>
            {
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
        };
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
            reporter,
            watch_namespace,
        }
    }
}

/// Read DEFAULT_IMAGE_<APP>_REPO and DEFAULT_IMAGE_<APP>_TAG env vars for each known app.
fn load_image_overrides() -> HashMap<String, ImageSpec> {
    let apps = [
        "sonarr",
        "radarr",
        "lidarr",
        "prowlarr",
        "sabnzbd",
        "transmission",
        "tautulli",
        "overseerr",
        "maintainerr",
        "jackett",
    ];

    let mut overrides = HashMap::new();

    for app in &apps {
        let repo_key = format!("DEFAULT_IMAGE_{}_REPO", app.to_uppercase());
        let tag_key = format!("DEFAULT_IMAGE_{}_TAG", app.to_uppercase());

        if let Ok(repo) = std::env::var(&repo_key) {
            let tag = std::env::var(&tag_key).unwrap_or_default();
            info!(%app, %repo, %tag, "loaded image override from env");
            overrides.insert(
                app.to_string(),
                ImageSpec {
                    repository: repo,
                    tag,
                    digest: String::new(),
                    pull_policy: "IfNotPresent".into(),
                },
            );
        }
    }

    overrides
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
                let overrides = load_image_overrides();
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
                let overrides = load_image_overrides();
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
                let overrides = load_image_overrides();
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
                let overrides = load_image_overrides();
                assert!(overrides.contains_key("prowlarr"));
                assert!(overrides.contains_key("sabnzbd"));
                assert_eq!(overrides.get("prowlarr").unwrap().tag, "latest");
                assert_eq!(overrides.get("sabnzbd").unwrap().tag, "3.7");
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
                let overrides = load_image_overrides();
                assert!(!overrides.contains_key("notanapp"));
            },
        );
    }

    // ── WATCH_ALL_NAMESPACES parsing (tested via Context::new internals) ──
    //
    // Context::new requires a kube::Client, which needs a real cluster.
    // We test the env-var parsing logic by extracting it into a helper here.

    /// Mirrors the WATCH_ALL_NAMESPACES parsing from Context::new.
    fn parse_watch_all() -> bool {
        match std::env::var("WATCH_ALL_NAMESPACES") {
            Ok(v)
                if v.eq_ignore_ascii_case("true") || v == "1" || v.eq_ignore_ascii_case("yes") =>
            {
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
            Ok(_) => false,
            Err(_) => false,
        }
    }

    #[test]
    fn watch_all_true_lowercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("true"), || {
            assert!(parse_watch_all());
        });
    }

    #[test]
    fn watch_all_true_uppercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("TRUE"), || {
            assert!(parse_watch_all());
        });
    }

    #[test]
    fn watch_all_one() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("1"), || {
            assert!(parse_watch_all());
        });
    }

    #[test]
    fn watch_all_yes() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("yes"), || {
            assert!(parse_watch_all());
        });
    }

    #[test]
    fn watch_all_false_lowercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("false"), || {
            assert!(!parse_watch_all());
        });
    }

    #[test]
    fn watch_all_false_uppercase() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("FALSE"), || {
            assert!(!parse_watch_all());
        });
    }

    #[test]
    fn watch_all_zero() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("0"), || {
            assert!(!parse_watch_all());
        });
    }

    #[test]
    fn watch_all_no() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("no"), || {
            assert!(!parse_watch_all());
        });
    }

    #[test]
    fn watch_all_empty_string() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some(""), || {
            assert!(!parse_watch_all());
        });
    }

    #[test]
    fn watch_all_unset() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", None::<&str>, || {
            assert!(!parse_watch_all());
        });
    }

    #[test]
    fn watch_all_unrecognized_defaults_false() {
        temp_env::with_var("WATCH_ALL_NAMESPACES", Some("maybe"), || {
            assert!(!parse_watch_all());
        });
    }

    // ── WATCH_NAMESPACE reading ──

    /// Mirrors the watch_namespace derivation from Context::new.
    fn derive_watch_namespace() -> Option<String> {
        let watch_all = parse_watch_all();
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
