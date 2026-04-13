use k8s_openapi::api::networking::v1::{
    IPBlock, NetworkPolicy, NetworkPolicyEgressRule, NetworkPolicyIngressRule, NetworkPolicyPeer,
    NetworkPolicyPort, NetworkPolicySpec,
};
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta};
use k8s_openapi::apimachinery::pkg::util::intstr::IntOrString;
use servarr_crds::{AppConfig, AppDefaults, AppType, NetworkPolicyConfig, ServarrApp};

use crate::common;

const DEFAULT_DENIED_CIDRS: &[&str] = &[
    "10.0.0.0/8",
    "172.16.0.0/12",
    "192.168.0.0/16",
    "169.254.0.0/16", // link-local, includes cloud metadata (169.254.169.254)
];

pub fn build(app: &ServarrApp) -> NetworkPolicy {
    let defaults = AppDefaults::for_app(&app.spec.app);
    let svc_spec = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let mut config = app.spec.network_policy_config.clone().unwrap_or_default();

    // SSH bastion: add NFS egress to private networks for NFS mount access
    if matches!(app.spec.app, AppType::SshBastion) {
        let persistence = app
            .spec
            .persistence
            .as_ref()
            .unwrap_or(&defaults.persistence);
        if !persistence.nfs_mounts.is_empty() {
            let nfs_rule = serde_json::json!({
                "to": [{
                    "ipBlock": {
                        "cidr": "10.0.0.0/8"
                    }
                }, {
                    "ipBlock": {
                        "cidr": "172.16.0.0/12"
                    }
                }, {
                    "ipBlock": {
                        "cidr": "192.168.0.0/16"
                    }
                }],
                "ports": [{
                    "protocol": "TCP",
                    "port": 2049
                }]
            });
            config.custom_egress_rules.push(nfs_rule);
        }
    }

    let app_ports: Vec<NetworkPolicyPort> = svc_spec
        .ports
        .iter()
        .map(|p| NetworkPolicyPort {
            port: Some(IntOrString::Int(p.port)),
            protocol: Some(p.protocol.clone()),
            ..Default::default()
        })
        .collect();

    // --- Ingress rules ---
    let ingress = build_ingress_rules(app, &config, &app_ports);

    // --- Egress rules ---
    let egress = build_egress_rules(&config);

    NetworkPolicy {
        metadata: ObjectMeta {
            name: Some(common::app_name(app)),
            namespace: Some(common::app_namespace(app)),
            labels: Some(common::labels(app)),
            owner_references: Some(vec![common::owner_reference(app)]),
            ..Default::default()
        },
        spec: Some(NetworkPolicySpec {
            pod_selector: Some(LabelSelector {
                match_labels: Some(common::selector_labels(app)),
                ..Default::default()
            }),
            ingress: Some(ingress),
            egress: Some(egress),
            policy_types: Some(vec!["Ingress".into(), "Egress".into()]),
        }),
    }
}

fn build_ingress_rules(
    app: &ServarrApp,
    config: &NetworkPolicyConfig,
    app_ports: &[NetworkPolicyPort],
) -> Vec<NetworkPolicyIngressRule> {
    let mut rules = Vec::new();

    // Allow from same namespace on app ports
    if config.allow_same_namespace {
        rules.push(NetworkPolicyIngressRule {
            from: Some(vec![NetworkPolicyPeer {
                pod_selector: Some(LabelSelector::default()),
                ..Default::default()
            }]),
            ports: Some(app_ports.to_vec()),
        });
    }

    // Allow from gateway namespace when gateway is enabled
    if let Some(ref gw) = app.spec.gateway
        && gw.enabled
    {
        for pr in &gw.parent_refs {
            if !pr.namespace.is_empty() {
                rules.push(NetworkPolicyIngressRule {
                    from: Some(vec![NetworkPolicyPeer {
                        namespace_selector: Some(LabelSelector {
                            match_labels: Some(
                                [(
                                    "kubernetes.io/metadata.name".to_string(),
                                    pr.namespace.clone(),
                                )]
                                .into_iter()
                                .collect(),
                            ),
                            ..Default::default()
                        }),
                        ..Default::default()
                    }]),
                    ports: Some(app_ports.to_vec()),
                });
            }
        }
    }

    // SSH bastion: allow SSH ingress from anywhere
    if matches!(app.spec.app, AppType::SshBastion) {
        rules.push(NetworkPolicyIngressRule {
            from: Some(vec![NetworkPolicyPeer {
                ip_block: Some(IPBlock {
                    cidr: "0.0.0.0/0".into(),
                    except: None,
                }),
                ..Default::default()
            }]),
            ports: Some(app_ports.to_vec()),
        });
    }

    // Allow peer port ingress from anywhere (Transmission torrent peers)
    if let Some(AppConfig::Transmission(ref tc)) = app.spec.app_config
        && let Some(ref peer) = tc.peer_port
    {
        rules.push(NetworkPolicyIngressRule {
            from: Some(vec![NetworkPolicyPeer {
                ip_block: Some(IPBlock {
                    cidr: "0.0.0.0/0".into(),
                    except: None,
                }),
                ..Default::default()
            }]),
            ports: Some(vec![
                NetworkPolicyPort {
                    protocol: Some("TCP".into()),
                    port: Some(IntOrString::Int(peer.port)),
                    ..Default::default()
                },
                NetworkPolicyPort {
                    protocol: Some("UDP".into()),
                    port: Some(IntOrString::Int(peer.port)),
                    ..Default::default()
                },
            ]),
        });
    }

    rules
}

fn build_egress_rules(config: &NetworkPolicyConfig) -> Vec<NetworkPolicyEgressRule> {
    let mut rules = Vec::new();

    // Allow same-namespace egress (pod-to-pod within namespace)
    rules.push(NetworkPolicyEgressRule {
        to: Some(vec![NetworkPolicyPeer {
            pod_selector: Some(LabelSelector::default()),
            ..Default::default()
        }]),
        ports: None,
    });

    // Allow DNS egress to kube-dns
    if config.allow_dns {
        rules.push(NetworkPolicyEgressRule {
            to: Some(vec![NetworkPolicyPeer {
                namespace_selector: Some(LabelSelector::default()),
                pod_selector: Some(LabelSelector {
                    match_labels: Some(
                        [("k8s-app".to_string(), "kube-dns".to_string())]
                            .into_iter()
                            .collect(),
                    ),
                    ..Default::default()
                }),
                ..Default::default()
            }]),
            ports: Some(vec![NetworkPolicyPort {
                protocol: Some("UDP".into()),
                port: Some(IntOrString::Int(53)),
                ..Default::default()
            }]),
        });
    }

    // Allow internet egress, blocking private CIDRs
    if config.allow_internet_egress {
        let except: Vec<String> = if config.denied_cidr_blocks.is_empty() {
            DEFAULT_DENIED_CIDRS.iter().map(|s| s.to_string()).collect()
        } else {
            config.denied_cidr_blocks.clone()
        };

        rules.push(NetworkPolicyEgressRule {
            to: Some(vec![NetworkPolicyPeer {
                ip_block: Some(IPBlock {
                    cidr: "0.0.0.0/0".into(),
                    except: Some(except),
                }),
                ..Default::default()
            }]),
            ports: None,
        });
    }

    // Custom egress rules (raw JSON)
    for (i, raw_rule) in config.custom_egress_rules.iter().enumerate() {
        match serde_json::from_value(raw_rule.clone()) {
            Ok(rule) => {
                tracing::debug!(index = i, "applied custom egress rule");
                rules.push(rule);
            }
            Err(e) => {
                tracing::warn!(
                    index = i,
                    error = %e,
                    "custom_egress_rules entry failed to parse as NetworkPolicyEgressRule; rule dropped"
                );
            }
        }
    }

    rules
}
