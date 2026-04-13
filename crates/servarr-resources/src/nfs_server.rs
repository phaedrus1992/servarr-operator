use k8s_openapi::api::apps::v1::{StatefulSet, StatefulSetSpec};
use k8s_openapi::api::core::v1::{
    Container, ContainerPort, EnvVar, PersistentVolumeClaim, PersistentVolumeClaimSpec, PodSpec,
    PodTemplateSpec, SecurityContext, VolumeMount, VolumeResourceRequirements,
};
use k8s_openapi::apimachinery::pkg::api::resource::Quantity;
use k8s_openapi::apimachinery::pkg::apis::meta::v1::{LabelSelector, ObjectMeta, OwnerReference};
use k8s_openapi::{
    api::core::v1::{Service, ServicePort, ServiceSpec},
    apimachinery::pkg::util::intstr::IntOrString,
};
use servarr_crds::NfsServerSpec;
use std::collections::BTreeMap;

const MANAGED_BY: &str = "servarr-operator";
const NFS_PORT: i32 = 2049;
const PORTMAPPER_PORT: i32 = 111;
const MOUNTD_PORT: i32 = 32767;
const COMPONENT: &str = "nfs-server";
const DEFAULT_IMAGE: &str = "erichough/nfs-server";
const EXPORT_DIR: &str = "/nfsshare";
// fsid=0 makes /nfsshare the NFSv4 pseudo-root.  Without it the kernel NFS
// server has no defined root, NFSv4 LOOKUP for /movies (etc.) fails, and
// clients fall back to NFSv3 which needs portmapper — unavailable on Docker
// Desktop.  With fsid=0 clients mount server:/movies and the server resolves
// it relative to /nfsshare.
const EXPORT_OPTS: &str = "*(rw,async,no_subtree_check,no_auth_nlm,insecure,no_root_squash,fsid=0)";
const DATA_VOLUME: &str = "data";

fn resource_name(stack_name: &str) -> String {
    format!("{stack_name}-nfs-server")
}

fn labels(stack_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("servarr.dev/stack".into(), stack_name.to_string()),
        ("servarr.dev/component".into(), COMPONENT.to_string()),
        ("app.kubernetes.io/managed-by".into(), MANAGED_BY.into()),
    ])
}

fn selector_labels(stack_name: &str) -> BTreeMap<String, String> {
    BTreeMap::from([
        ("servarr.dev/stack".into(), stack_name.to_string()),
        ("servarr.dev/component".into(), COMPONENT.to_string()),
    ])
}

/// Build the StatefulSet for the in-cluster NFS server.
///
/// The StatefulSet runs a single NFS server pod backed by a PVC whose size
/// and storage class are taken from `nfs`. The pod exports `EXPORT_DIR` via
/// NFS on port 2049.
pub fn build_statefulset(
    stack_name: &str,
    namespace: &str,
    nfs: &NfsServerSpec,
    owner_ref: OwnerReference,
) -> StatefulSet {
    let name = resource_name(stack_name);
    let labels = labels(stack_name);
    let selector = selector_labels(stack_name);

    let image = nfs
        .image
        .as_ref()
        .map(|img| {
            let tag = if img.tag.is_empty() {
                "latest".to_string()
            } else {
                img.tag.clone()
            };
            format!("{}:{tag}", img.repository)
        })
        .unwrap_or_else(|| DEFAULT_IMAGE.to_string());

    let storage_class = nfs.storage_class.clone().filter(|s| !s.is_empty());

    let volume_claim_template = PersistentVolumeClaim {
        metadata: ObjectMeta {
            name: Some(DATA_VOLUME.to_string()),
            ..Default::default()
        },
        spec: Some(PersistentVolumeClaimSpec {
            access_modes: Some(vec!["ReadWriteOnce".to_string()]),
            storage_class_name: storage_class,
            resources: Some(VolumeResourceRequirements {
                requests: Some(BTreeMap::from([(
                    "storage".to_string(),
                    Quantity(nfs.storage_size.clone()),
                )])),
                ..Default::default()
            }),
            ..Default::default()
        }),
        ..Default::default()
    };

    StatefulSet {
        metadata: ObjectMeta {
            name: Some(name.clone()),
            namespace: Some(namespace.to_string()),
            labels: Some(labels.clone()),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(StatefulSetSpec {
            replicas: Some(1),
            service_name: Some(name),
            selector: LabelSelector {
                match_labels: Some(selector.clone()),
                ..Default::default()
            },
            template: PodTemplateSpec {
                metadata: Some(ObjectMeta {
                    labels: Some(selector),
                    ..Default::default()
                }),
                spec: Some(PodSpec {
                    init_containers: Some(vec![Container {
                        name: "mkdir".to_string(),
                        image: Some("busybox:latest".to_string()),
                        image_pull_policy: Some("IfNotPresent".to_string()),
                        command: Some(vec!["sh".to_string(), "-c".to_string(), {
                            let dirs = [
                                nfs.movies_path.as_str(),
                                nfs.tv_path.as_str(),
                                nfs.music_path.as_str(),
                                nfs.movies_4k_path.as_str(),
                                nfs.tv_4k_path.as_str(),
                            ]
                            .iter()
                            .map(|p| format!("{EXPORT_DIR}{p}"))
                            .collect::<Vec<_>>()
                            .join(" ");
                            format!("mkdir -p {dirs}")
                        }]),
                        volume_mounts: Some(vec![VolumeMount {
                            name: DATA_VOLUME.to_string(),
                            mount_path: EXPORT_DIR.to_string(),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }]),
                    containers: vec![Container {
                        name: COMPONENT.to_string(),
                        image: Some(image),
                        image_pull_policy: Some("IfNotPresent".to_string()),
                        env: Some(vec![EnvVar {
                            name: "NFS_EXPORT_0".to_string(),
                            value: Some(format!("{EXPORT_DIR} {EXPORT_OPTS}")),
                            ..Default::default()
                        }]),
                        ports: Some(vec![
                            ContainerPort {
                                name: Some("nfs".to_string()),
                                container_port: NFS_PORT,
                                protocol: Some("TCP".to_string()),
                                ..Default::default()
                            },
                            ContainerPort {
                                name: Some("portmapper-tcp".to_string()),
                                container_port: PORTMAPPER_PORT,
                                protocol: Some("TCP".to_string()),
                                ..Default::default()
                            },
                            ContainerPort {
                                name: Some("portmapper-udp".to_string()),
                                container_port: PORTMAPPER_PORT,
                                protocol: Some("UDP".to_string()),
                                ..Default::default()
                            },
                            ContainerPort {
                                name: Some("mountd-tcp".to_string()),
                                container_port: MOUNTD_PORT,
                                protocol: Some("TCP".to_string()),
                                ..Default::default()
                            },
                            ContainerPort {
                                name: Some("mountd-udp".to_string()),
                                container_port: MOUNTD_PORT,
                                protocol: Some("UDP".to_string()),
                                ..Default::default()
                            },
                        ]),
                        security_context: Some(SecurityContext {
                            privileged: Some(true),
                            ..Default::default()
                        }),
                        volume_mounts: Some(vec![VolumeMount {
                            name: DATA_VOLUME.to_string(),
                            mount_path: EXPORT_DIR.to_string(),
                            ..Default::default()
                        }]),
                        ..Default::default()
                    }],
                    ..Default::default()
                }),
            },
            volume_claim_templates: Some(vec![volume_claim_template]),
            ..Default::default()
        }),
        ..Default::default()
    }
}

/// Build the headless Service for the in-cluster NFS server.
///
/// Other pods reach the NFS server via the cluster-local DNS name
/// `{stack-name}-nfs-server.{namespace}.svc.cluster.local` on port 2049.
pub fn build_service(stack_name: &str, namespace: &str, owner_ref: OwnerReference) -> Service {
    let name = resource_name(stack_name);

    Service {
        metadata: ObjectMeta {
            name: Some(name),
            namespace: Some(namespace.to_string()),
            labels: Some(labels(stack_name)),
            owner_references: Some(vec![owner_ref]),
            ..Default::default()
        },
        spec: Some(ServiceSpec {
            // Headless (clusterIP: None) so DNS returns the pod IP directly.
            // The kubelet runs in the host network namespace where ClusterIP
            // iptables rules don't apply; pod IPs are routable but virtual
            // ClusterIPs are not.
            cluster_ip: Some("None".to_string()),
            selector: Some(selector_labels(stack_name)),
            ports: Some(vec![
                ServicePort {
                    name: Some("nfs".to_string()),
                    port: NFS_PORT,
                    target_port: Some(IntOrString::Int(NFS_PORT)),
                    protocol: Some("TCP".to_string()),
                    ..Default::default()
                },
                ServicePort {
                    name: Some("portmapper-tcp".to_string()),
                    port: PORTMAPPER_PORT,
                    target_port: Some(IntOrString::Int(PORTMAPPER_PORT)),
                    protocol: Some("TCP".to_string()),
                    ..Default::default()
                },
                ServicePort {
                    name: Some("portmapper-udp".to_string()),
                    port: PORTMAPPER_PORT,
                    target_port: Some(IntOrString::Int(PORTMAPPER_PORT)),
                    protocol: Some("UDP".to_string()),
                    ..Default::default()
                },
                ServicePort {
                    name: Some("mountd-tcp".to_string()),
                    port: MOUNTD_PORT,
                    target_port: Some(IntOrString::Int(MOUNTD_PORT)),
                    protocol: Some("TCP".to_string()),
                    ..Default::default()
                },
                ServicePort {
                    name: Some("mountd-udp".to_string()),
                    port: MOUNTD_PORT,
                    target_port: Some(IntOrString::Int(MOUNTD_PORT)),
                    protocol: Some("UDP".to_string()),
                    ..Default::default()
                },
            ]),
            ..Default::default()
        }),
        ..Default::default()
    }
}
