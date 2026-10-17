//! Derived Secret that holds Transmission's salted `rpc-password` (#848).
//!
//! Transmission accepts `rpc-password` in a salted form and keeps it as-is. The pod mounts the
//! derived Secret as `password`, so the plaintext never reaches `settings.json`.

use super::*;
use k8s_openapi::api::core::v1::ObjectReference;
use rand::distr::{Alphanumeric, SampleString as _};
use servarr_resources::common::{RPC_PASSWORD_SECRET_SUFFIX, metadata};
use sha1::{Digest, Sha1};

/// Key of the salted password in the derived Secret. The pod mounts it as the `password` file.
const PASSWORD_KEY: &str = "password";

/// Length of the salt that [`random_salt`] makes. Transmission reads any salt length.
const SALT_LEN: usize = 8;

/// Length of the hex SHA-1 digest in the salted form.
const DIGEST_HEX_LEN: usize = 40;

/// Transmission's salted form: `{`, the hex SHA-1 of the password then the salt, and the salt.
/// This matches `tr_ssha1` in upstream `libtransmission/crypto-utils.cc`.
fn salted(password: &str, salt: &str) -> String {
    // Transmission's stored password format requires SHA-1.
    // nosemgrep: rust.lang.security.insecure-hashes.insecure-hashes
    let mut hasher = Sha1::new();
    hasher.update(password.as_bytes());
    hasher.update(salt.as_bytes());
    format!("{{{}{salt}", hex::encode(hasher.finalize()))
}

/// Alphanumeric characters are a subset of the alphabet that Transmission itself uses for salts.
fn random_salt() -> String {
    Alphanumeric.sample_string(&mut rand::rng(), SALT_LEN)
}

/// True when `hash` is a salted form that was made from `password`.
fn matches(hash: &str, password: &str) -> bool {
    hash.strip_prefix('{')
        .and_then(|rest| rest.get(DIGEST_HEX_LEN..))
        .is_some_and(|salt| salted(password, salt) == hash)
}

/// The hash to publish. A hash that still matches the password stays, so the salt is stable and
/// a reconcile does not change the Secret.
fn desired_hash(existing: Option<&str>, password: &str) -> String {
    match existing {
        Some(hash) if matches(hash, password) => hash.to_string(),
        _ => salted(password, &random_salt()),
    }
}

/// True when `secret` has an owner reference to `app`. The operator writes only Secrets it owns.
fn is_owned_by(secret: &Secret, app: &ServarrApp) -> bool {
    let Some(uid) = app.metadata.uid.as_ref() else {
        return false;
    };
    secret
        .metadata
        .owner_references
        .as_ref()
        .is_some_and(|refs| refs.iter().any(|r| &r.uid == uid))
}

/// The admin password, or `None` after a Warning Event when the Secret is missing or malformed.
/// A transient API error returns `Err`, so the reconcile retries.
async fn read_admin_password(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    recorder: &Recorder,
    obj_ref: &ObjectReference,
) -> Result<Option<String>, Error> {
    let Some(ref ac) = app.spec.admin_credentials else {
        return Ok(None);
    };
    let err = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "password").await {
        Ok(password) => return Ok(Some(password)),
        Err(err) => err,
    };
    warn!(app = %app.name_any(), error = %err.log_summary(),
        "rpc-password: cannot read the admin password");
    match err {
        servarr_api::SecretError::Kube(e) if !servarr_api::k8s::is_kube_not_found(&e) => {
            Err(Error::Kube(e))
        }
        other => {
            publish_event(
                recorder,
                obj_ref,
                TenantEvent {
                    type_: EventType::Warning,
                    reason: "AdminCredentialsUnreadable",
                    note: TenantSafeMessage::new(format!(
                        "The pod cannot start until the adminCredentials Secret has a readable \
                         password: {}",
                        other.public_summary()
                    )),
                    action: "EnsureRpcPassword",
                },
            )
            .await;
            Ok(None)
        }
    }
}

/// Create or update the derived Secret before the Deployment mounts it. A pod that mounts a
/// missing Secret waits in `ContainerCreating`.
///
/// A missing or malformed admin Secret publishes a Warning Event and skips the write. The rest of
/// the reconcile still runs, and a later reconcile writes the Secret once the admin Secret is
/// readable. A transient API error, a failed write, and an existing Secret that this app does
/// not own all return an error.
pub(super) async fn ensure_rpc_password_secret(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
    recorder: &Recorder,
    obj_ref: &ObjectReference,
) -> Result<(), Error> {
    if app.spec.app != AppType::Transmission || app.spec.admin_credentials.is_none() {
        return Ok(());
    }
    let Some(password) = read_admin_password(client, app, ns, recorder, obj_ref).await? else {
        return Ok(());
    };

    let secret_api = Api::<Secret>::namespaced(client.clone(), ns);
    let name = servarr_resources::common::child_name(app, RPC_PASSWORD_SECRET_SUFFIX);
    let existing = secret_api.get_opt(&name).await.map_err(Error::Kube)?;
    if existing.as_ref().is_some_and(|s| !is_owned_by(s, app)) {
        return Err(Error::SecretNotOwned(name));
    }
    let existing_hash = existing
        .as_ref()
        .and_then(|s| s.data.as_ref())
        .and_then(|d| d.get(PASSWORD_KEY))
        .and_then(|b| std::str::from_utf8(&b.0).ok());
    let hash = desired_hash(existing_hash, &password);
    if existing_hash == Some(hash.as_str()) {
        return Ok(());
    }
    if existing.is_some() {
        debug!(app = %app.name_any(), secret = %name,
            "rpc-password: the stored hash is stale or unreadable, replacing it");
    }

    let secret = Secret {
        metadata: metadata(app, RPC_PASSWORD_SECRET_SUFFIX),
        string_data: Some(std::collections::BTreeMap::from([(
            PASSWORD_KEY.to_string(),
            hash,
        )])),
        type_: Some("Opaque".into()),
        ..Default::default()
    };
    info!(app = %app.name_any(), secret = %name, "writing the salted rpc-password secret");
    secret_api
        .patch(
            &name,
            &PatchParams::apply(FIELD_MANAGER).force(),
            &Patch::Apply(&secret),
        )
        .await
        .map_err(Error::Kube)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testutils::build_mock_client;
    use k8s_openapi::ByteString;
    use k8s_openapi::apimachinery::pkg::apis::meta::v1::OwnerReference;
    use proptest::prelude::*;
    use serde_json::json;
    use servarr_crds::{AdminCredentialsSpec, ServarrAppSpec};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    /// Vectors from upstream `tests/libtransmission/crypto-test.cc`. A mismatch means the hash
    /// no longer matches what Transmission accepts.
    #[test]
    fn matches_upstream_known_answers() {
        assert!(matches(
            "{15ad0621b259a84d24dcd4e75b09004e98a3627bAMbyRHJy",
            "test"
        ));
        assert!(matches(
            "{10e2d7acbb104d970514a147cd16d51dfa40fb3c0OSwJtOL",
            "QNY)(*#$B)!_X$B !_B#($^!)*&$%CV!#)&$C!@$(P*)"
        ));
        assert!(matches("{a94a8fe5ccb19ba61c4c0873d391e987982fbbd3", "test"));
        assert!(matches(
            "{d209a21d3bc4f8fc4f8faf347e69f3def597eb170pySy4ai1ZPMjeU1",
            "test"
        ));
    }

    #[test]
    fn salted_reproduces_upstream_known_answer() {
        assert_eq!(
            salted("test", "AMbyRHJy"),
            "{15ad0621b259a84d24dcd4e75b09004e98a3627bAMbyRHJy"
        );
    }

    #[test]
    fn matches_rejects_wrong_password_and_malformed_hashes() {
        let hash = "{15ad0621b259a84d24dcd4e75b09004e98a3627bAMbyRHJy";
        assert!(!matches(hash, "tesT"));
        assert!(!matches("", "test"));
        assert!(!matches(
            "15ad0621b259a84d24dcd4e75b09004e98a3627bAMbyRHJy",
            "test"
        ));
        assert!(!matches("{15ad06", "test"));
        // A cut inside a multi-byte character must not panic.
        assert!(!matches(&format!("{{{}é", "a".repeat(39)), "test"));
    }

    #[test]
    fn desired_hash_keeps_a_matching_hash_and_replaces_a_stale_one() {
        let kept = "{15ad0621b259a84d24dcd4e75b09004e98a3627bAMbyRHJy";
        assert_eq!(desired_hash(Some(kept), "test"), kept);
        let replaced = desired_hash(Some(kept), "rotated");
        assert!(matches(&replaced, "rotated"));
        assert_ne!(replaced, kept);
        assert!(matches(&desired_hash(None, "test"), "test"));
    }

    proptest! {
        #[test]
        fn a_salted_hash_matches_only_its_password(
            password in ".{0,64}",
            salt in "[0-9a-zA-Z./]{0,16}",
        ) {
            let hash = salted(&password, &salt);
            let wrong = format!("{password}x");
            prop_assert!(matches(&hash, &password));
            prop_assert!(!matches(&hash, &wrong));
        }

        /// A stored value comes from a Secret that anyone with write access can change.
        #[test]
        fn matches_never_panics_and_only_accepts_its_own_form(
            hash in ".{0,120}",
            password in ".{0,32}",
        ) {
            if matches(&hash, &password) {
                let salt = &hash[1 + DIGEST_HEX_LEN..];
                prop_assert_eq!(salted(&password, salt), hash);
            }
        }

        #[test]
        fn desired_hash_is_stable_for_its_own_output(password in ".{0,64}") {
            let first = desired_hash(None, &password);
            prop_assert_eq!(desired_hash(Some(&first), &password), first);
        }

        #[test]
        fn desired_hash_replaces_a_hash_of_another_password(
            old in ".{0,32}",
            new in ".{0,32}",
        ) {
            prop_assume!(old != new);
            let stale = salted(&old, "AMbyRHJy");
            let fresh = desired_hash(Some(&stale), &new);
            prop_assert!(matches(&fresh, &new));
            prop_assert_ne!(fresh, stale);
        }
    }

    const NS: &str = "test";
    const ADMIN_SECRET: &str = "tx-admin";
    const APP_UID: &str = "uid-tx";
    /// Base64 of `secret`.
    const SECRET_B64: &str = "c2VjcmV0";
    const EVENTS_PATH: &str = "/apis/events.k8s.io/v1/namespaces/test/events";

    fn transmission_app(admin: Option<&str>) -> ServarrApp {
        ServarrApp {
            metadata: kube::api::ObjectMeta {
                name: Some("tx".into()),
                namespace: Some(NS.into()),
                uid: Some(APP_UID.into()),
                ..Default::default()
            },
            spec: ServarrAppSpec {
                app: AppType::Transmission,
                admin_credentials: admin.map(|n| AdminCredentialsSpec {
                    secret_name: n.into(),
                }),
                ..Default::default()
            },
            status: None,
        }
    }

    fn derived_name(app: &ServarrApp) -> String {
        servarr_resources::common::child_name(app, RPC_PASSWORD_SECRET_SUFFIX)
    }

    fn secret_url(name: &str) -> String {
        format!("/api/v1/namespaces/{NS}/secrets/{name}")
    }

    fn status_body(code: u16, reason: &str) -> serde_json::Value {
        json!({
            "apiVersion": "v1", "kind": "Status", "status": "Failure",
            "reason": reason, "code": code, "message": "details"
        })
    }

    fn recorder_and_ref(client: &Client) -> (Recorder, ObjectReference) {
        let recorder = Recorder::new(
            client.clone(),
            kube::runtime::events::Reporter {
                controller: "servarr-operator".into(),
                instance: None,
            },
        );
        let obj_ref = ObjectReference {
            kind: Some("ServarrApp".into()),
            name: Some("tx".into()),
            namespace: Some(NS.into()),
            uid: Some(APP_UID.into()),
            ..Default::default()
        };
        (recorder, obj_ref)
    }

    async fn run(client: &Client, app: &ServarrApp) -> Result<(), Error> {
        let (recorder, obj_ref) = recorder_and_ref(client);
        ensure_rpc_password_secret(client, app, NS, &recorder, &obj_ref).await
    }

    async fn mount_admin_secret(server: &MockServer, data: serde_json::Value) {
        Mock::given(method("GET"))
            .and(path(secret_url(ADMIN_SECRET)))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "v1", "kind": "Secret",
                "metadata": { "name": ADMIN_SECRET, "namespace": NS },
                "data": data
            })))
            .mount(server)
            .await;
    }

    async fn mount_default_admin_secret(server: &MockServer) {
        mount_admin_secret(
            server,
            json!({ "username": "YWRtaW4=", "password": SECRET_B64 }),
        )
        .await;
    }

    /// An existing derived Secret. `owner_uid` sets who owns it, and `None` leaves it unowned.
    async fn mount_derived_get(
        server: &MockServer,
        name: &str,
        existing_hash: Option<&str>,
        owner_uid: Option<&str>,
    ) {
        let response = match existing_hash {
            None => ResponseTemplate::new(404).set_body_json(status_body(404, "NotFound")),
            Some(hash) => {
                let secret = Secret {
                    metadata: kube::api::ObjectMeta {
                        name: Some(name.into()),
                        namespace: Some(NS.into()),
                        owner_references: owner_uid.map(|uid| {
                            vec![OwnerReference {
                                uid: uid.into(),
                                ..Default::default()
                            }]
                        }),
                        ..Default::default()
                    },
                    data: Some(std::collections::BTreeMap::from([(
                        PASSWORD_KEY.to_string(),
                        ByteString(hash.as_bytes().to_vec()),
                    )])),
                    ..Default::default()
                };
                ResponseTemplate::new(200).set_body_json(secret)
            }
        };
        Mock::given(method("GET"))
            .and(path(secret_url(name)))
            .respond_with(response)
            .mount(server)
            .await;
    }

    async fn mount_derived_patch(server: &MockServer, name: &str, expected_calls: u64) {
        Mock::given(method("PATCH"))
            .and(path(secret_url(name)))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "v1", "kind": "Secret",
                "metadata": { "name": name, "namespace": NS }
            })))
            .expect(expected_calls)
            .mount(server)
            .await;
    }

    async fn mount_events(server: &MockServer, expected_calls: u64) {
        Mock::given(method("POST"))
            .and(path(EVENTS_PATH))
            .respond_with(ResponseTemplate::new(201).set_body_json(json!({
                "apiVersion": "events.k8s.io/v1", "kind": "Event",
                "metadata": { "name": "e", "namespace": NS }
            })))
            .expect(expected_calls)
            .mount(server)
            .await;
    }

    /// The published `stringData.password` of the one PATCH the server received.
    async fn patched_hash(server: &MockServer) -> String {
        let requests = server.received_requests().await.unwrap_or_default();
        let patch = requests
            .iter()
            .find(|r| r.method.as_str() == "PATCH")
            .expect("the operator must patch the derived Secret");
        let body: serde_json::Value = serde_json::from_slice(&patch.body).unwrap();
        body["stringData"][PASSWORD_KEY]
            .as_str()
            .unwrap()
            .to_string()
    }

    #[tokio::test]
    async fn creates_the_derived_secret_with_a_hash_of_the_admin_password() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_default_admin_secret(&server).await;
        mount_derived_get(&server, &name, None, None).await;
        mount_derived_patch(&server, &name, 1).await;

        run(&client, &app).await.unwrap();

        let hash = patched_hash(&server).await;
        assert!(
            matches(&hash, "secret"),
            "published hash must match the password: {hash}"
        );
        assert!(!hash.contains("secret"), "plaintext must not be published");
        server.verify().await;
    }

    #[tokio::test]
    async fn leaves_the_derived_secret_alone_while_the_password_is_unchanged() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_default_admin_secret(&server).await;
        let hash = salted("secret", "AMbyRHJy");
        mount_derived_get(&server, &name, Some(&hash), Some(APP_UID)).await;
        mount_derived_patch(&server, &name, 0).await;

        run(&client, &app).await.unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn rewrites_the_derived_secret_after_the_password_rotates() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_default_admin_secret(&server).await;
        let stale = salted("old-password", "AMbyRHJy");
        mount_derived_get(&server, &name, Some(&stale), Some(APP_UID)).await;
        mount_derived_patch(&server, &name, 1).await;

        run(&client, &app).await.unwrap();

        assert!(matches(&patched_hash(&server).await, "secret"));
        server.verify().await;
    }

    #[tokio::test]
    async fn refuses_to_write_a_secret_that_the_app_does_not_own() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_default_admin_secret(&server).await;
        mount_derived_get(&server, &name, Some("a user value"), None).await;
        mount_derived_patch(&server, &name, 0).await;

        let err = run(&client, &app).await.unwrap_err();

        assert!(
            matches!(err, Error::SecretNotOwned(ref n) if n == &name),
            "got {err:?}"
        );
        server.verify().await;
    }

    #[tokio::test]
    async fn refuses_a_secret_owned_by_another_app() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_default_admin_secret(&server).await;
        mount_derived_get(&server, &name, Some("x"), Some("uid-someone-else")).await;

        let err = run(&client, &app).await.unwrap_err();

        assert!(matches!(err, Error::SecretNotOwned(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn warns_and_skips_the_write_when_the_admin_secret_is_missing() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        Mock::given(method("GET"))
            .and(path(secret_url(ADMIN_SECRET)))
            .respond_with(ResponseTemplate::new(404).set_body_json(status_body(404, "NotFound")))
            .mount(&server)
            .await;
        mount_derived_patch(&server, &derived_name(&app), 0).await;
        mount_events(&server, 1).await;

        run(&client, &app).await.unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn warns_and_skips_the_write_when_the_admin_secret_has_no_password_key() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        mount_admin_secret(&server, json!({ "username": "YWRtaW4=" })).await;
        mount_derived_patch(&server, &derived_name(&app), 0).await;
        mount_events(&server, 1).await;

        run(&client, &app).await.unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn retries_instead_of_warning_when_the_admin_secret_read_fails_transiently() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        Mock::given(method("GET"))
            .and(path(secret_url(ADMIN_SECRET)))
            .respond_with(
                ResponseTemplate::new(500).set_body_json(status_body(500, "InternalError")),
            )
            .mount(&server)
            .await;
        mount_derived_patch(&server, &derived_name(&app), 0).await;
        mount_events(&server, 0).await;

        let err = run(&client, &app).await.unwrap_err();

        assert!(matches!(err, Error::Kube(_)), "got {err:?}");
        server.verify().await;
    }

    #[tokio::test]
    async fn fails_when_the_derived_secret_cannot_be_written() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_default_admin_secret(&server).await;
        mount_derived_get(&server, &name, None, None).await;
        Mock::given(method("PATCH"))
            .and(path(secret_url(&name)))
            .respond_with(ResponseTemplate::new(403).set_body_json(status_body(403, "Forbidden")))
            .mount(&server)
            .await;

        let err = run(&client, &app).await.unwrap_err();

        assert!(matches!(err, Error::Kube(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn does_nothing_without_admin_credentials_or_for_another_app() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let mut sonarr = transmission_app(Some(ADMIN_SECRET));
        sonarr.spec.app = AppType::Sonarr;

        run(&client, &transmission_app(None)).await.unwrap();
        run(&client, &sonarr).await.unwrap();

        let requests = server.received_requests().await.unwrap_or_default();
        assert!(
            requests.is_empty(),
            "no API call expected, got {}",
            requests.len()
        );
    }
}
