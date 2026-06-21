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
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use tempfile::TempDir;

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

/// Create a fresh temp directory unique to this test. The returned `TempDir` guard
/// removes it on drop (including on panic); the `PathBuf` is its canonicalized path,
/// so the allowlist (literal) and the script's `realpath` agree even if the system
/// temp dir contains a symlink component (e.g. macOS `/tmp` -> `/private/tmp`).
fn fresh_dir(tag: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::Builder::new()
        .prefix(&format!("rsync-wrap-{tag}-"))
        .tempdir()
        .expect("create temp dir");
    let canonical = fs::canonicalize(dir.path()).expect("canonicalize temp dir");
    (dir, canonical)
}

/// Create `parent/name` and return its path.
fn make_dir(parent: &Path, name: &str) -> PathBuf {
    let p = parent.join(name);
    fs::create_dir_all(&p).expect("create dir");
    p
}

fn write_exec(path: &Path, body: &str) {
    fs::write(path, body).expect("write stub");
    fs::set_permissions(path, fs::Permissions::from_mode(0o755)).expect("chmod stub");
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
    fs::write(&script_path, script).expect("write wrapper");

    let bin = workdir.join("bin");
    fs::create_dir_all(&bin).expect("create bin dir");
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
        .unwrap_or_else(|e| panic!("failed to run bash {}: {e}", script_path.display()));

    // A signal kill yields no exit code; treating that as a normal failure would make
    // the `assert_ne!(status, 0)` rejection tests pass vacuously, so fail loudly.
    if let Some(sig) = out.status.signal() {
        panic!(
            "bash killed by signal {sig}; stderr:\n{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    RunResult {
        status: out.status.code().expect("exit code present (no signal)"),
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&out.stderr).into_owned(),
    }
}

#[test]
fn path_with_spaces_is_passed_to_rsync_intact() {
    let (_tmp, wd) = fresh_dir("space");
    let tv = make_dir(&wd, "tv");
    make_dir(&tv, "Taskmaster AU");

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
    let (_tmp, wd) = fresh_dir("glob");
    let tv = make_dir(&wd, "tv");
    make_dir(&tv, "TaskA");
    make_dir(&tv, "TaskB");

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
    let (_tmp, wd) = fresh_dir("inject-chain");
    let tv = make_dir(&wd, "tv");
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
    let (_tmp, wd) = fresh_dir("inject-subst");
    let tv = make_dir(&wd, "tv");
    let pwned = wd.join("pwned");

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
fn brace_and_tilde_metacharacters_are_rejected() {
    let (_tmp, wd) = fresh_dir("metachar");
    let tv = make_dir(&wd, "tv");
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);

    // Brace expansion could fan a single request into a huge argument list.
    let brace = format!("rsync --server --sender . {}/{{a,b,c}}", tv.display());
    assert_ne!(
        run_wrapper(&wd, &script, &brace).status,
        0,
        "brace expansion must be rejected"
    );

    // Tilde expansion could resolve outside the intended tree.
    let res = run_wrapper(&wd, &script, "rsync --server --sender . ~root/x");
    assert_ne!(res.status, 0, "tilde expansion must be rejected");
}

#[test]
fn non_rsync_command_is_rejected() {
    let (_tmp, wd) = fresh_dir("non-rsync");
    let script = gen_script(SshMode::RestrictedRsync, vec![wd.display().to_string()]);
    let res = run_wrapper(&wd, &script, "cat /etc/passwd");
    assert_ne!(res.status, 0, "non-rsync command must be rejected");
}

#[test]
fn write_without_sender_is_rejected() {
    let (_tmp, wd) = fresh_dir("write");
    let tv = make_dir(&wd, "tv");
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!("rsync --server . {}/", tv.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(res.status, 0, "write (missing --sender) must be rejected");
}

#[test]
fn path_traversal_is_rejected() {
    let (_tmp, wd) = fresh_dir("traversal");
    let tv = make_dir(&wd, "tv");
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!("rsync --server --sender . {}/../etc", tv.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(res.status, 0, "path traversal must be rejected");
}

#[test]
fn path_outside_allowlist_is_rejected() {
    let (_tmp, wd) = fresh_dir("outside");
    let tv = make_dir(&wd, "tv");
    let secret = make_dir(&wd, "secret");
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!("rsync --server --sender . {}", secret.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(res.status, 0, "path outside allowlist must be rejected");
}

#[test]
fn nonexistent_path_outside_allowlist_is_rejected() {
    let (_tmp, wd) = fresh_dir("nonexist");
    let tv = make_dir(&wd, "tv");
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    // Path does not exist on the server and is outside the allowlist; exercises the
    // `else` branch where realpath is skipped and the literal path is checked.
    let cmd = format!("rsync --server --sender . {}/elsewhere/show", wd.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(
        res.status, 0,
        "non-existent path outside allowlist must be rejected"
    );
}

#[test]
fn disallowed_flag_log_file_is_rejected() {
    let (_tmp, wd) = fresh_dir("flaglist");
    let tv = make_dir(&wd, "tv");
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    let cmd = format!(
        "rsync --server --sender --log-file=/tmp/x . {}",
        tv.display()
    );
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(
        res.status, 0,
        "--log-file must be rejected by flag allowlist; stderr: {}",
        res.stderr
    );
}

#[test]
fn non_flag_arg_before_dot_separator_is_rejected() {
    let (_tmp, wd) = fresh_dir("bareflag");
    let tv = make_dir(&wd, "tv");
    let script = gen_script(SshMode::RestrictedRsync, vec![tv.display().to_string()]);
    // A bare word (non-flag) before the "." path separator must not pass through.
    let cmd = format!("rsync --server --sender inject . {}", tv.display());
    let res = run_wrapper(&wd, &script, &cmd);
    assert_ne!(
        res.status, 0,
        "bare word before path separator must be rejected; stderr: {}",
        res.stderr
    );
}

#[test]
fn rsync_mode_allows_any_path_and_parses_spaces() {
    let (_tmp, wd) = fresh_dir("anymode");
    make_dir(&wd, "Some Show");

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
