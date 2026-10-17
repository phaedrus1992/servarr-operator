//! Deployment builder tests for the SSH bastion app.

use k8s_openapi::api::apps::v1::Deployment;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::*;

/// Build with no image overrides.
fn build_deployment(app: &ServarrApp) -> Deployment {
    servarr_resources::deployment::build(app, &std::collections::HashMap::new()).unwrap()
}

#[test]
fn test_deployment_ssh_bastion_init_containers() {
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "backup".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::RestrictedRsync,
                    restricted_rsync: Some(RestrictedRsyncConfig {
                        allowed_paths: vec!["/data".into()],
                    }),
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = build_deployment(&app);
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();

    // Should have init containers: generate-host-keys, copy-authorized-keys, and patch-entry.
    // copy-authorized-keys stages the Secret (world-writable tmpfs) into an emptyDir with
    // correct permissions so sshd StrictModes accepts the directory.
    let init = pod_spec.init_containers.as_ref().unwrap();
    assert!(
        init.iter().any(|c| c.name == "generate-host-keys"),
        "SSH bastion should have generate-host-keys init container"
    );
    assert!(
        init.iter().any(|c| c.name == "copy-authorized-keys"),
        "SSH bastion should have copy-authorized-keys init container"
    );
    // Script must dereference symlinks (POSIX cp without -r), not copy them as-is (BusyBox cp -r
    // preserves symlinks from secret mounts → broken symlinks in the emptyDir).
    let copy_script = init
        .iter()
        .find(|c| c.name == "copy-authorized-keys")
        .and_then(|c| c.command.as_ref())
        .and_then(|cmd| cmd.last())
        .expect("copy-authorized-keys must have a shell script as its last command arg");
    assert!(
        !copy_script.contains("cp -r"),
        "copy-authorized-keys script must not use 'cp -r' (BusyBox preserves symlinks from \
         secret mounts); use a for-loop with non-recursive cp to dereference them"
    );
    assert!(
        copy_script.contains("for f in /etc/authorized_keys.src/*"),
        "copy-authorized-keys script must iterate files and dereference symlinks via plain cp"
    );
    assert!(
        init.iter().any(|c| c.name == "patch-entry"),
        "SSH bastion should have patch-entry init container"
    );

    let container = &pod_spec.containers[0];
    let mounts = container.volume_mounts.as_ref().unwrap();

    // authorized-keys-src: Secret staged at a separate path, read-only
    let src_mount = mounts
        .iter()
        .find(|m| m.name == "authorized-keys-src")
        .expect("SSH bastion should have authorized-keys-src volume mount");
    assert_eq!(src_mount.mount_path, "/etc/authorized_keys.src");
    assert_eq!(
        src_mount.read_only,
        Some(true),
        "Secret staging mount must be read-only"
    );

    // authorized-keys: emptyDir populated by copy-authorized-keys init container, writable
    let ak_mount = mounts
        .iter()
        .find(|m| m.name == "authorized-keys")
        .expect("SSH bastion should have authorized-keys directory volume mount");
    assert_eq!(
        ak_mount.mount_path, "/etc/authorized_keys",
        "authorized-keys should mount at /etc/authorized_keys"
    );
    assert!(
        !ak_mount.read_only.unwrap_or(false),
        "authorized-keys emptyDir must be writable so init container can populate it"
    );
    assert!(
        ak_mount.sub_path.is_none(),
        "authorized-keys must be a directory mount, not a subPath file"
    );

    // Should have restricted-rsync volume mount
    assert!(
        mounts.iter().any(|m| m.name == "restricted-rsync"),
        "SSH bastion in RestrictedRsync mode should have restricted-rsync volume mount"
    );

    // Volumes should include authorized-keys-src secret, authorized-keys emptyDir, and restricted-rsync configmap
    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(
        volumes.iter().any(|v| v.name == "authorized-keys-src"),
        "Should have authorized-keys-src Secret volume"
    );
    assert!(
        volumes.iter().any(|v| v.name == "authorized-keys"),
        "Should have authorized-keys emptyDir volume"
    );
    assert!(
        volumes.iter().any(|v| v.name == "restricted-rsync"),
        "Should have restricted-rsync volume"
    );

    // args should include -p with the SSH port so sshd listens on the right port
    let args = container
        .args
        .as_ref()
        .expect("SSH bastion should have args");
    let p_idx = args
        .iter()
        .position(|a| a == "-p")
        .expect("args should include -p flag");
    assert!(
        p_idx + 1 < args.len(),
        "args should have a port value after -p"
    );
}

#[test]
fn test_deployment_ssh_bastion_rsync_mode_uses_restricted_rsync() {
    // SshMode::Rsync must mount the restricted-rsync script and use it as the user shell.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-rsync-deploy".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "backup".into(),
                    uid: 1000,
                    gid: 1000,
                    mode: SshMode::Rsync,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = build_deployment(&app);
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let container = &pod_spec.containers[0];
    let mounts = container.volume_mounts.as_ref().unwrap();

    assert!(
        mounts.iter().any(|m| m.name == "restricted-rsync"),
        "SshMode::Rsync must mount the restricted-rsync script"
    );

    let volumes = pod_spec.volumes.as_ref().unwrap();
    assert!(
        volumes.iter().any(|v| v.name == "restricted-rsync"),
        "SshMode::Rsync must have restricted-rsync volume"
    );

    // Shell must be the restricted-rsync wrapper
    let env = container.env.as_ref().unwrap();
    let ssh_users = env.iter().find(|e| e.name == "SSH_USERS").unwrap();
    assert!(
        ssh_users
            .value
            .as_deref()
            .unwrap_or("")
            .contains("/usr/local/bin/restricted-rsync-backup"),
        "SshMode::Rsync user shell must be /usr/local/bin/restricted-rsync-backup"
    );
}

#[test]
fn test_deployment_ssh_bastion_shell_package_installed() {
    // A user with /bin/bash should trigger apk add bash in the patch-entry script.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-shell-pkg".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "alice".into(),
                    uid: 1001,
                    gid: 1001,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: Some("/bin/bash".into()),
                    public_keys: String::new(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = build_deployment(&app);
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let init = pod_spec.init_containers.as_ref().unwrap();
    let patch = init.iter().find(|c| c.name == "patch-entry").unwrap();
    let script = patch.command.as_ref().unwrap().last().unwrap();
    assert!(
        script.contains("apk add"),
        "patch-entry should install packages"
    );
    assert!(
        script.contains("bash"),
        "patch-entry should install bash for /bin/bash users"
    );
    // A failed install must stop the init with the cause, not continue silently (#857).
    assert!(
        !script.contains("|| true"),
        "a failed shell install must not be swallowed"
    );
    assert!(
        script.contains("apk add failed") && script.contains("exit 1"),
        "a failed shell install must exit with the cause"
    );
}

#[test]
fn test_deployment_ssh_bastion_default_shell_no_install() {
    // Users with no shell (defaults to /bin/sh) must NOT trigger apk installs.
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-ssh-sh-pkg".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "bob".into(),
                    uid: 1002,
                    gid: 1002,
                    mode: SshMode::Shell,
                    restricted_rsync: None,
                    shell: None,
                    public_keys: String::new(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };

    let deploy = build_deployment(&app);
    let pod_spec = deploy.spec.unwrap().template.spec.unwrap();
    let init = pod_spec.init_containers.as_ref().unwrap();
    let patch = init.iter().find(|c| c.name == "patch-entry").unwrap();
    let script = patch.command.as_ref().unwrap().last().unwrap();
    assert!(
        !script.contains("apk add --no-cache bash")
            && !script.contains("apk add --no-cache zsh")
            && !script.contains("apk add --no-cache fish"),
        "patch-entry must not install shell packages when all users use /bin/sh"
    );
}
