use k8s_openapi::api::core::v1::{Service, ServicePort, ServiceSpec};
use servarr_crds::{AppConfig, AppDefaults, AppType, ServarrApp};

use crate::common;

pub fn build(app: &ServarrApp) -> Service {
    let defaults = AppDefaults::for_app(&app.spec.app);
    let svc = app.spec.service.as_ref().unwrap_or(&defaults.service);
    let app_config = app
        .spec
        .app_config
        .as_ref()
        .or(defaults.app_config.as_ref());

    let mut ports: Vec<ServicePort> = svc
        .ports
        .iter()
        .map(|p| ServicePort {
            name: Some(p.name.clone()),
            port: p.port,
            protocol: Some(p.protocol.clone()),
            ..Default::default()
        })
        .collect();

    // Transmission peer port
    if let (AppType::Transmission, Some(AppConfig::Transmission(tc))) = (&app.spec.app, app_config)
        && let Some(pp) = &tc.peer_port
    {
        ports.push(ServicePort {
            name: Some("peer-tcp".into()),
            port: pp.port,
            protocol: Some("TCP".into()),
            ..Default::default()
        });
        ports.push(ServicePort {
            name: Some("peer-udp".into()),
            port: pp.port,
            protocol: Some("UDP".into()),
            ..Default::default()
        });
    }

    Service {
        metadata: common::metadata(app, ""),
        spec: Some(ServiceSpec {
            type_: Some(svc.service_type.clone()),
            selector: Some(common::selector_labels(app)),
            ports: Some(ports),
            ..Default::default()
        }),
        ..Default::default()
    }
}
