//! Admin credentials wiring of the Transmission deployment: env vars, volume, and scripts (#848).

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::*;

fn transmission(admin_secret: Option<&str>) -> ServarrApp {
    ServarrApp {
        metadata: ObjectMeta {
            name: Some("tx".into()),
            namespace: Some("media".into()),
            uid: Some("uid-tx".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::Transmission,
            admin_credentials: admin_secret.map(|name| AdminCredentialsSpec {
                secret_name: name.into(),
            }),
            ..Default::default()
        },
        status: None,
    }
}

fn admin_volume(app: &ServarrApp) -> Option<k8s_openapi::api::core::v1::Volume> {
    let deploy = servarr_resources::deployment::build(app, &std::collections::HashMap::new())
        .expect("the Transmission deployment must build");
    deploy
        .spec?
        .template
        .spec?
        .volumes?
        .into_iter()
        .find(|v| v.name == "admin-credentials")
}

#[test]
fn password_file_comes_from_the_derived_secret_not_the_admin_secret() {
    let app = transmission(Some("tx-admin"));
    let volume = admin_volume(&app).expect("the admin-credentials volume must exist");

    assert!(
        volume.secret.is_none(),
        "the plaintext Secret must not be mounted whole"
    );
    let sources = volume
        .projected
        .expect("the volume must be projected")
        .sources;
    let sources = sources.expect("the projection must list its sources");
    let parts: Vec<(String, String)> = sources
        .iter()
        .filter_map(|s| s.secret.as_ref())
        .map(|s| {
            let items = s.items.as_ref().expect("each source must pick one key");
            assert_eq!(items.len(), 1, "each source must project one key only");
            (s.name.clone(), items[0].path.clone())
        })
        .collect();

    let derived = servarr_resources::common::child_name(
        &app,
        servarr_resources::common::RPC_PASSWORD_SECRET_SUFFIX,
    );
    assert_eq!(
        parts,
        vec![
            ("tx-admin".to_string(), "username".to_string()),
            (derived, "password".to_string()),
        ]
    );
}

#[test]
fn no_admin_volume_without_admin_credentials() {
    assert!(admin_volume(&transmission(None)).is_none());
}

fn make_app(app_type: AppType) -> ServarrApp {
    ServarrApp {
        metadata: ObjectMeta {
            name: Some("test-app".into()),
            namespace: Some("media".into()),
            uid: Some("test-uid-123".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: app_type,
            ..Default::default()
        },
        status: None,
    }
}

fn find_env<'a>(
    env: &'a [k8s_openapi::api::core::v1::EnvVar],
    name: &str,
) -> Option<&'a k8s_openapi::api::core::v1::EnvVar> {
    env.iter().find(|e| e.name == name)
}

fn get_env(app: &ServarrApp) -> Vec<k8s_openapi::api::core::v1::EnvVar> {
    let deploy =
        servarr_resources::deployment::build(app, &std::collections::HashMap::new()).unwrap();
    deploy
        .spec
        .unwrap()
        .template
        .spec
        .unwrap()
        .containers
        .into_iter()
        .next()
        .unwrap()
        .env
        .unwrap_or_default()
}

fn build_deployment(app: &ServarrApp) -> k8s_openapi::api::apps::v1::Deployment {
    servarr_resources::deployment::build(app, &std::collections::HashMap::new()).unwrap()
}

#[test]
fn test_admin_credentials_transmission_env_vars() {
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let env = get_env(&app);

    // Sonarr-style env vars must NOT be set for Transmission
    assert!(find_env(&env, "TRANSMISSION__AUTH__USERNAME").is_none());
    assert!(find_env(&env, "TRANSMISSION__AUTH__PASSWORD").is_none());
    assert!(find_env(&env, "TRANSMISSION__AUTH__METHOD").is_none());

    // FILE__USER / FILE__PASS: LSIO FILE__ mechanism for s6 container env
    let file_user = find_env(&env, "FILE__USER").expect("FILE__USER must be set for Transmission");
    let file_pass = find_env(&env, "FILE__PASS").expect("FILE__PASS must be set for Transmission");
    assert_eq!(
        file_user.value.as_deref(),
        Some("/run/secrets/admin/username"),
        "FILE__USER must point to the mounted secret file"
    );
    assert_eq!(
        file_pass.value.as_deref(),
        Some("/run/secrets/admin/password"),
        "FILE__PASS must point to the mounted secret file"
    );

    // USER/PASS secretKeyRef kept for exec probe (curl -u "$USER:$PASS")
    let user = find_env(&env, "USER").expect("USER env var should be set for Transmission");
    let pass = find_env(&env, "PASS").expect("PASS env var should be set for Transmission");
    assert_eq!(
        user.value_from
            .as_ref()
            .and_then(|v| v.secret_key_ref.as_ref())
            .map(|s| s.key.as_str()),
        Some("username"),
        "USER should be sourced from adminCredentials secret key 'username'"
    );
    assert_eq!(
        pass.value_from
            .as_ref()
            .and_then(|v| v.secret_key_ref.as_ref())
            .map(|s| s.key.as_str()),
        Some("password"),
        "PASS should be sourced from adminCredentials secret key 'password'"
    );
}

#[test]
fn test_admin_credentials_transmission_mounts_secret_volume() {
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let deploy = build_deployment(&app);
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    // Volume must exist
    let vol = pod_spec
        .volumes
        .as_ref()
        .and_then(|vs| vs.iter().find(|v| v.name == "admin-credentials"))
        .expect("admin-credentials volume must be present");
    let sources = vol.projected.as_ref().and_then(|p| p.sources.as_ref());
    let first = sources.and_then(|s| s.first()?.secret.as_ref());
    assert_eq!(
        first.map(|s| s.name.as_str()),
        Some("creds"),
        "username source"
    );

    // Volume mount must exist in the container
    let mounts = pod_spec.containers[0]
        .volume_mounts
        .as_ref()
        .expect("container must have volume mounts");
    let mount = mounts
        .iter()
        .find(|m| m.name == "admin-credentials")
        .expect("admin-credentials mount must be present");
    assert_eq!(mount.mount_path, "/run/secrets/admin");
    assert_eq!(mount.read_only, Some(true));
}

#[test]
fn test_admin_credentials_transmission_mounts_auth_script_to_custom_cont_init() {
    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let deploy = build_deployment(&app);
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    // The custom-cont-init.d script must be mounted in the main container via subPath.
    // This guarantees the auth script runs AFTER init-transmission-config
    // (confirmed by s6-rc dependency chain: init-transmission-config →
    // init-config-end → init-mods → init-mods-package-install →
    // init-mods-end → init-custom-files).
    let mounts = pod_spec.containers[0]
        .volume_mounts
        .as_ref()
        .expect("container must have volume mounts");
    let auth_mount = mounts
        .iter()
        .find(|m| m.mount_path == "/custom-cont-init.d/99-transmission-auth.sh")
        .expect("99-transmission-auth.sh must be mounted at /custom-cont-init.d/");
    assert_eq!(auth_mount.name, "scripts");
    assert_eq!(
        auth_mount.sub_path.as_deref(),
        Some("99-transmission-auth.sh")
    );
    assert_eq!(auth_mount.read_only, Some(true));

    // The script key must also be present in the scripts volume items
    let scripts_vol = pod_spec
        .volumes
        .as_ref()
        .and_then(|vs| vs.iter().find(|v| v.name == "scripts"))
        .expect("scripts volume must be present");
    let items = scripts_vol
        .config_map
        .as_ref()
        .and_then(|cm| cm.items.as_ref())
        .expect("scripts volume must have items");
    assert!(
        items.iter().any(|i| i.key == "99-transmission-auth.sh"),
        "scripts volume items must include 99-transmission-auth.sh"
    );
}

#[test]
fn test_admin_credentials_transmission_uses_exec_probe() {
    use k8s_openapi::api::core::v1::ExecAction;

    let mut app = make_app(AppType::Transmission);
    app.spec.admin_credentials = Some(AdminCredentialsSpec {
        secret_name: "creds".into(),
    });
    let deploy = build_deployment(&app);
    let container = &deploy.spec.unwrap().template.spec.unwrap().containers[0];

    let liveness = container
        .liveness_probe
        .as_ref()
        .expect("liveness probe must be set");
    let readiness = container
        .readiness_probe
        .as_ref()
        .expect("readiness probe must be set");

    // Both probes must be exec (not httpGet) so they send credentials after auth is enabled
    assert!(
        liveness.http_get.is_none(),
        "liveness probe must not be httpGet when adminCredentials is set"
    );
    assert!(
        liveness.exec.is_some(),
        "liveness probe must be exec when adminCredentials is set"
    );
    let exec_cmd = liveness.exec.as_ref().and_then(|e| e.command.as_ref());
    assert!(
        exec_cmd.is_some_and(|c| c.iter().any(|s| s.contains("USER") && s.contains("PASS"))),
        "exec probe command must include $USER and $PASS"
    );
    assert!(
        readiness.exec.is_some(),
        "readiness probe must be exec when adminCredentials is set"
    );
    let _ = ExecAction::default(); // suppress unused import warning
}
