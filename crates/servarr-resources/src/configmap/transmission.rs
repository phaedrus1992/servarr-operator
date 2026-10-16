//! Default Transmission `settings.json` written by the init container (#834).

pub(super) fn default_transmission_settings() -> String {
    #[expect(
        clippy::unwrap_used,
        reason = "serde_json::json! literal is always serializable"
    )]
    serde_json::to_string_pretty(&serde_json::json!({
        "download-dir": "/downloads/complete",
        "incomplete-dir": "/downloads/incomplete",
        "incomplete-dir-enabled": true,
        "dht-enabled": true,
        "pex-enabled": true,
        "lpd-enabled": false,
        "encryption": 1,
        "speed-limit-down-enabled": false,
        "speed-limit-up-enabled": false,
        "ratio-limit-enabled": false,
        "download-queue-enabled": true,
        "download-queue-size": 5,
        "seed-queue-enabled": true,
        "seed-queue-size": 10,
        "rpc-host-whitelist-enabled": false,
        "rpc-whitelist-enabled": true,
        "rpc-whitelist": "127.0.0.1,::1,10.*,172.*,192.168.*",
        "cache-size-mb": 4,
        "umask": "002",
        "rename-partial-files": true,
        "start-added-torrents": true,
    }))
    .unwrap()
}
