//! Behavioral tests for the generated restricted-rsync wrapper script (issue #107).
//!
//! The wrapper is mounted as a user's login shell in the SSH bastion and is invoked
//! by sshd as `shell -c "rsync --server --sender ... . <path>"`. It must therefore
//! perform the word-splitting and glob expansion a real login shell would, while
//! still enforcing read-only access and the path allowlist.
//!
//! These tests generate the script via the builder, then actually execute it under
//! `bash` with a stub `rsync` on `PATH` that echoes its arguments. This is the only
//! way to catch parsing regressions (e.g. spaces collapsing to the last token, or
//! globs reaching `rsync` unexpanded) — string matching the script cannot.
#![cfg(unix)]

use k8s_openapi::apimachinery::pkg::apis::meta::v1::ObjectMeta;
use servarr_crds::*;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

/// Generate the wrapper script for a single user with the given mode + allowed paths.
fn gen_script(mode: SshMode, allowed_paths: Vec<String>) -> String {
    let restricted_rsync =
        (mode == SshMode::RestrictedRsync).then_some(RestrictedRsyncConfig { allowed_paths });
    let app = ServarrApp {
        metadata: ObjectMeta {
            name: Some("bastion".into()),
            namespace: Some("infra".into()),
            uid: Some("uid-107".into()),
            ..Default::default()
        },
        spec: ServarrAppSpec {
            app: AppType::SshBastion,
            app_config: Some(AppConfig::SshBastion(SshBastionConfig {
                users: vec![SshUser {
                    name: "backup".into(),
                    uid: 1000,
                    gid: 1000,
                    mode,
                    restricted_rsync,
                    shell: None,
                    public_keys: "ssh-ed25519 AAAA".into(),
                }],
                ..Default::default()
            })),
            ..Default::default()
        },
        status: None,
    };
    let cm = servarr_resources::configmap::build_ssh_bastion_restricted_rsync(&app)
        .expect("rsync-mode user must produce a ConfigMap");
    cm.data
        .expect("ConfigMap must have data")
        .remove("restricted-rsync-backup.sh")
        .expect("script key must exist")
}

/// Create a fresh, canonicalized temp directory unique to this test run.
fn fresh_dir(tag: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!("rsync-wrap-{tag}-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    // Canonicalize so the allowlist (literal) and realpath() in the script agree
    // even if temp_dir() contains a symlink component.
    fs::canonicalize(&dir).unwrap()
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
}

struct RunResult {
    status: i32,
    stdout: String,
    stderr: String,
}

/// Write the script to `workdir`, stub out `rsync`/`logger` on PATH, and run the
/// wrapper as a login shell: `bash wrapper.sh -c "<cmd>"`.
fn run_wrapper(workdir: &Path, script: &str, cmd: &str) -> RunResult {
    let script_path = workdir.join("wrapper.sh");
    fs::write(&script_path, script).unwrap();

    let bin = workdir.join("bin");
    fs::create_dir_all(&bin).unwrap();
    // Stub rsync echoes each received argument so tests can assert on exact parsing.
    write_exec(
        &bin.join("rsync"),
        "#!/bin/bash\nfor a in \"$@\"; do printf 'ARG:%s\\n' \"$a\"; done\n",
    );
    // Stub logger so the script's syslog calls don't fail under `set -e`.
    write_exec(&bin.join("logger"), "#!/bin/bash\nexit 0\n");

    let out = Command::new("bash")
        .arg(&script_path)
        .arg("-c")
        .arg(cmd)
        .env("PATH", format!("{}:/usr/bin:/bin", bin.display()))
        .env_remove("SSH_ORIGINAL_COMMAND")
        .current_dir(workdir)
        .output()
        .expect("failed to run bash");

    RunResult {
        status: out.status.code().unwrap_or(-1),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn path_with_spaces_is_passed_to_rsync_intact() {
    let wd = fresh_dir("space");
    let tv = wd.join("tv");
    let show = tv.join("Taskmaster AU");
    fs::create_dir_all(&show).unwrap();

    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    // rsync escapes the embedded space with a backslash in the remote command.
    let cmd = format!(
        r"rsync --server --sender -e.x . {}/Taskmaster\ AU/",
        tv.display()
    );
    let res = run_wrapper(&wd, &script, &cmd);

    let expected = format!("ARG:{}/Taskmaster AU/", tv.display());
    assert_eq!(
        res.status, 0,
        "spaced path should be accepted; stderr: {}",
        res.stderr
    );
    assert!(
        res.stdout.contains(&expected),
        "rsync should receive the full spaced path '{expected}', got:\n{}",
        res.stdout
    );
}

#[test]
fn glob_is_expanded_before_reaching_rsync() {
    let wd = fresh_dir("glob");
    let tv = wd.join("tv");
    fs::create_dir_all(tv.join("TaskA")).unwrap();
    fs::create_dir_all(tv.join("TaskB")).unwrap();

    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!("rsync --server --sender -e.x . {}/Task*", tv.display());
    let res = run_wrapper(&wd, &script, &cmd);

    assert_eq!(
        res.status, 0,
        "globbed path should be accepted; stderr: {}",
        res.stderr
    );
    assert!(
        res.stdout.contains(&format!("ARG:{}/TaskA", tv.display())),
        "glob should expand to TaskA, got:\n{}",
        res.stdout
    );
    assert!(
        res.stdout.contains(&format!("ARG:{}/TaskB", tv.display())),
        "glob should expand to TaskB, got:\n{}",
        res.stdout
    );
}

#[test]
fn command_chaining_is_rejected() {
    let wd = fresh_dir("inject-chain");
    let tv = wd.join("tv");
    fs::create_dir_all(&tv).unwrap();
    let pwned = wd.join("pwned");

    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!(
        "rsync --server --sender . {tv}; touch {pwned}",
        tv = tv.display(),
        pwned = pwned.display()
    );
    let res = run_wrapper(&wd, &script, &cmd);

    assert_ne!(res.status, 0, "chained command must be rejected");
    assert!(!pwned.exists(), "injected `touch` must not have executed");
}

#[test]
fn command_substitution_is_rejected() {
    let wd = fresh_dir("inject-subst");
    let tv = wd.join("tv");
    fs::create_dir_all(&tv).unwrap();
    let pwned = wd.join("pwned2");

    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!(
        "rsync --server --sender . {tv}/$(touch {pwned})",
        tv = tv.display(),
        pwned = pwned.display()
    );
    let res = run_wrapper(&wd, &script, &cmd);

    assert_ne!(res.status, 0, "command substitution must be rejected");
    assert!(
        !pwned.exists(),
        "command substitution must not have executed"
    );
}

#[test]
fn non_rsync_command_is_rejected() {
    let wd = fresh_dir("non-rsync");
    let script = gen_script(SshMode::RestrictedRsync, vec![wd.display().to_string()]);
    let res = run_wrapper(&wd, &script, "cat /etc/passwd");
    assert_ne!(res.status, 0, "non-rsync command must be rejected");
}

#[test]
fn write_without_sender_is_rejected() {
    let wd = fresh_dir("write");
    let tv = wd.join("tv");
    fs::create_dir_all(&tv).unwrap();
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!("rsync --server . {}/", tv.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(res.status, 0, "write (missing --sender) must be rejected");
}

#[test]
fn path_traversal_is_rejected() {
    let wd = fresh_dir("traversal");
    let tv = wd.join("tv");
    fs::create_dir_all(&tv).unwrap();
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!("rsync --server --sender . {}/../etc", tv.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(res.status, 0, "path traversal must be rejected");
}

#[test]
fn path_outside_allowlist_is_rejected() {
    let wd = fresh_dir("outside");
    let tv = wd.join("tv");
    let secret = wd.join("secret");
    fs::create_dir_all(&tv).unwrap();
    fs::create_dir_all(&secret).unwrap();
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!("rsync --server --sender . {}", secret.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(res.status, 0, "path outside allowlist must be rejected");
}

#[test]
fn rsync_mode_allows_any_path_and_parses_spaces() {
    let wd = fresh_dir("anymode");
    let show = wd.join("Some Show");
    fs::create_dir_all(&show).unwrap();

    // Empty allowlist => SshMode::Rsync: any path permitted, but parsing still applies.
    let script = gen_script(SshMode::Rsync, vec![]);
    let cmd = format!(
        r"rsync --server --sender -e.x . {}/Some\ Show/",
        wd.display()
    );
    let res = run_wrapper(&wd, &script, &cmd);

    assert_eq!(
        res.status, 0,
        "rsync mode should allow any path; stderr: {}",
        res.stderr
    );
    assert!(
        res.stdout
            .contains(&format!("ARG:{}/Some Show/", wd.display())),
        "rsync mode should still parse spaced paths, got:\n{}",
        res.stdout
    );
}
