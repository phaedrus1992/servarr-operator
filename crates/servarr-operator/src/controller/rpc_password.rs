//! Derived Secret that holds Transmission's salted `rpc-password` (#848).
//!
//! Transmission accepts `rpc-password` in a salted form and keeps it as-is. The pod mounts the
//! derived Secret as `password`, so the plaintext never reaches `settings.json`.

use super::*;
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
    let Some(rest) = hash.strip_prefix('{') else {
        return false;
    };
    let Some(salt) = rest.get(DIGEST_HEX_LEN..) else {
        return false;
    };
    salted(password, salt) == hash
}

/// The hash to publish. A hash that still matches the password stays, so the salt is stable and
/// a reconcile does not change the Secret.
fn desired_hash(existing: Option<&str>, password: &str) -> String {
    match existing {
        Some(hash) if matches(hash, password) => hash.to_string(),
        _ => salted(password, &random_salt()),
    }
}

/// Create or update the derived Secret before the Deployment mounts it. A pod that mounts a
/// missing Secret waits in `ContainerCreating`.
///
/// An unreadable admin Secret only logs a warning and skips the write. The rest of the reconcile
/// still runs, and the `CredentialReadError` condition reports the cause. A later reconcile
/// writes the Secret once the admin Secret can be read. A failed write returns an error.
pub(super) async fn ensure_rpc_password_secret(
    client: &Client,
    app: &ServarrApp,
    ns: &str,
) -> Result<(), Error> {
    if app.spec.app != AppType::Transmission {
        return Ok(());
    }
    let Some(ref ac) = app.spec.admin_credentials else {
        return Ok(());
    };
    let password = match servarr_api::read_secret_key(client, ns, &ac.secret_name, "password").await
    {
        Ok(password) => password,
        Err(e) => {
            warn!(app = %app.name_any(), error = %e.log_summary(),
                    "rpc-password: cannot read the admin password, skipping the derived Secret");
            return Ok(());
        }
    };

    let secret_api = Api::<Secret>::namespaced(client.clone(), ns);
    let name = servarr_resources::common::child_name(app, RPC_PASSWORD_SECRET_SUFFIX);
    let existing = secret_api.get_opt(&name).await.map_err(Error::Kube)?;
    let existing_hash = existing
        .as_ref()
        .and_then(|s| s.data.as_ref())
        .and_then(|d| d.get(PASSWORD_KEY))
        .and_then(|b| std::str::from_utf8(&b.0).ok());
    let hash = desired_hash(existing_hash, &password);
    if existing_hash == Some(hash.as_str()) {
        return Ok(());
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
    use proptest::prelude::*;

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

    use crate::testutils::build_mock_client;
    use k8s_openapi::ByteString;
    use serde_json::json;
    use servarr_crds::{AdminCredentialsSpec, ServarrAppSpec};
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const NS: &str = "test";
    const ADMIN_SECRET: &str = "tx-admin";
    /// Base64 of `secret`.
    const SECRET_B64: &str = "c2VjcmV0";

    fn transmission_app(admin: Option<&str>) -> ServarrApp {
        ServarrApp {
            metadata: kube::api::ObjectMeta {
                name: Some("tx".into()),
                namespace: Some(NS.into()),
                uid: Some("uid-tx".into()),
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

    async fn mount_admin_secret(server: &MockServer) {
        Mock::given(method("GET"))
            .and(path(secret_url(ADMIN_SECRET)))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "apiVersion": "v1", "kind": "Secret",
                "metadata": { "name": ADMIN_SECRET, "namespace": NS },
                "data": { "username": "YWRtaW4=", "password": SECRET_B64 }
            })))
            .mount(server)
            .await;
    }

    async fn mount_derived_get(server: &MockServer, name: &str, existing_hash: Option<&str>) {
        let response = match existing_hash {
            None => ResponseTemplate::new(404).set_body_json(json!({
                "apiVersion": "v1", "kind": "Status", "status": "Failure",
                "reason": "NotFound", "code": 404, "message": "not found"
            })),
            Some(hash) => {
                let secret = Secret {
                    metadata: kube::api::ObjectMeta {
                        name: Some(name.into()),
                        namespace: Some(NS.into()),
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
        mount_admin_secret(&server).await;
        mount_derived_get(&server, &name, None).await;
        mount_derived_patch(&server, &name, 1).await;

        ensure_rpc_password_secret(&client, &app, NS).await.unwrap();

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
        mount_admin_secret(&server).await;
        mount_derived_get(&server, &name, Some(&salted("secret", "AMbyRHJy"))).await;
        mount_derived_patch(&server, &name, 0).await;

        ensure_rpc_password_secret(&client, &app, NS).await.unwrap();
        server.verify().await;
    }

    #[tokio::test]
    async fn rewrites_the_derived_secret_after_the_password_rotates() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_admin_secret(&server).await;
        mount_derived_get(&server, &name, Some(&salted("old-password", "AMbyRHJy"))).await;
        mount_derived_patch(&server, &name, 1).await;

        ensure_rpc_password_secret(&client, &app, NS).await.unwrap();

        assert!(matches(&patched_hash(&server).await, "secret"));
        server.verify().await;
    }

    #[tokio::test]
    async fn skips_the_write_when_the_admin_secret_is_missing() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        Mock::given(method("GET"))
            .and(path(secret_url(ADMIN_SECRET)))
            .respond_with(ResponseTemplate::new(404).set_body_json(json!({
                "apiVersion": "v1", "kind": "Status", "status": "Failure",
                "reason": "NotFound", "code": 404, "message": "secrets \"tx-admin\" not found"
            })))
            .mount(&server)
            .await;
        mount_derived_patch(&server, &derived_name(&app), 0).await;

        ensure_rpc_password_secret(&client, &app, NS).await.unwrap();

        server.verify().await;
    }

    #[tokio::test]
    async fn fails_when_the_derived_secret_cannot_be_written() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let app = transmission_app(Some(ADMIN_SECRET));
        let name = derived_name(&app);
        mount_admin_secret(&server).await;
        mount_derived_get(&server, &name, None).await;
        Mock::given(method("PATCH"))
            .and(path(secret_url(&name)))
            .respond_with(ResponseTemplate::new(403).set_body_json(json!({
                "apiVersion": "v1", "kind": "Status", "status": "Failure",
                "reason": "Forbidden", "code": 403, "message": "forbidden"
            })))
            .mount(&server)
            .await;

        let err = ensure_rpc_password_secret(&client, &app, NS)
            .await
            .unwrap_err();

        assert!(matches!(err, Error::Kube(_)), "got {err:?}");
    }

    #[tokio::test]
    async fn does_nothing_without_admin_credentials_or_for_another_app() {
        let server = MockServer::start().await;
        let client = build_mock_client(&server.uri()).await;
        let mut sonarr = transmission_app(Some(ADMIN_SECRET));
        sonarr.spec.app = AppType::Sonarr;

        ensure_rpc_password_secret(&client, &transmission_app(None), NS)
            .await
            .unwrap();
        ensure_rpc_password_secret(&client, &sonarr, NS)
            .await
            .unwrap();

        let requests = server.received_requests().await.unwrap_or_default();
        assert!(
            requests.is_empty(),
            "no API call expected, got {}",
            requests.len()
        );
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
    }
}
