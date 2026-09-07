//! mDNS / DNS-SD device discovery (`_unishare._tcp.local.`).
//!
//! Why mDNS: standard (Bonjour / Avahi / Windows 10+), pure-Rust
//! implementation (`mdns-sd`) that needs no system daemon, works on Linux,
//! Windows and macOS, and carries metadata in TXT records (device name,
//! version, port, TLS fingerprint, whether a PIN is required).

use anyhow::{Context, Result};
use mdns_sd::{ResolvedService, ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::net::IpAddr;
use std::time::{Duration, Instant};

pub const SERVICE_TYPE: &str = "_unishare._tcp.local.";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Device {
    pub name: String,
    pub addresses: Vec<IpAddr>,
    pub port: u16,
    pub fingerprint: String,
    pub version: String,
    pub requires_pin: bool,
    pub online: bool,
}

impl Device {
    /// Preferred address: first IPv4, else first address.
    pub fn best_addr(&self) -> Option<IpAddr> {
        self.addresses
            .iter()
            .find(|a| a.is_ipv4())
            .or_else(|| self.addresses.first())
            .copied()
    }
}

/// Keeps an mDNS announcement alive while in scope.
pub struct Announcer {
    daemon: ServiceDaemon,
    fullname: String,
}

impl Announcer {
    pub fn start(device_name: &str, port: u16, fingerprint: &str, requires_pin: bool) -> Result<Self> {
        let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
        let mut props: HashMap<String, String> = HashMap::new();
        props.insert("name".to_string(), device_name.to_string());
        props.insert("fp".to_string(), fingerprint.to_string());
        props.insert("ver".to_string(), crate::APP_VERSION.to_string());
        props.insert("proto".to_string(), super::protocol::PROTOCOL_VERSION.to_string());
        props.insert("pin".to_string(), if requires_pin { "1" } else { "0" }.to_string());
        let instance = sanitize_instance(device_name);
        let host = format!("{}.local.", instance.replace(' ', "-"));
        let info = ServiceInfo::new(SERVICE_TYPE, &instance, &host, "", port, props)
            .context("building service info")?
            .enable_addr_auto();
        let fullname = info.get_fullname().to_string();
        daemon.register(info).context("registering mDNS service")?;
        tracing::info!(%fullname, port, "mDNS service announced");
        Ok(Self { daemon, fullname })
    }
}

impl Drop for Announcer {
    fn drop(&mut self) {
        if let Ok(rx) = self.daemon.unregister(&self.fullname) {
            let _ = rx.recv_timeout(Duration::from_millis(500));
        }
        let _ = self.daemon.shutdown();
    }
}

fn sanitize_instance(name: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| !matches!(c, '.' | '\u{0}'))
        .take(60)
        .collect();
    if s.trim().is_empty() { "uni-share".into() } else { s }
}

fn device_from_info(info: &ResolvedService) -> Device {
    let prop = |k: &str| info.txt_properties.get_property_val_str(k).unwrap_or("").to_string();
    let mut addrs: Vec<IpAddr> = info.get_addresses().iter().map(|a| a.to_ip_addr()).collect();
    addrs.sort();
    let name = {
        let n = prop("name");
        if n.is_empty() {
            info.get_fullname()
                .strip_suffix(&format!(".{SERVICE_TYPE}"))
                .unwrap_or(info.get_fullname())
                .to_string()
        } else {
            n
        }
    };
    Device {
        name,
        addresses: addrs,
        port: info.get_port(),
        fingerprint: prop("fp"),
        version: prop("ver"),
        requires_pin: prop("pin") == "1",
        online: true,
    }
}

/// Browse the network for `timeout`, returning unique devices (by fingerprint,
/// falling back to name). Devices seen then removed are marked `online=false`.
pub async fn discover(timeout: Duration, exclude_fingerprint: Option<&str>) -> Result<Vec<Device>> {
    let daemon = ServiceDaemon::new().context("starting mDNS daemon")?;
    let receiver = daemon.browse(SERVICE_TYPE).context("browsing mDNS")?;
    let deadline = Instant::now() + timeout;
    let mut found: BTreeMap<String, Device> = BTreeMap::new();

    loop {
        let now = Instant::now();
        if now >= deadline {
            break;
        }
        let remaining = deadline - now;
        // recv_async is available in mdns-sd (flume channel).
        match tokio::time::timeout(remaining, receiver.recv_async()).await {
            Ok(Ok(ServiceEvent::ServiceResolved(info))) => {
                let d = device_from_info(&info);
                if exclude_fingerprint.is_some_and(|fp| !fp.is_empty() && fp == d.fingerprint) {
                    continue;
                }
                let key = if d.fingerprint.is_empty() { d.name.clone() } else { d.fingerprint.clone() };
                found
                    .entry(key)
                    .and_modify(|e| {
                        for a in &d.addresses {
                            if !e.addresses.contains(a) {
                                e.addresses.push(*a);
                            }
                        }
                        e.online = true;
                    })
                    .or_insert(d);
            }
            Ok(Ok(ServiceEvent::ServiceRemoved(_, fullname))) => {
                for d in found.values_mut() {
                    if fullname.starts_with(&sanitize_instance(&d.name)) {
                        d.online = false;
                    }
                }
            }
            Ok(Ok(_)) => {}
            Ok(Err(e)) => {
                tracing::debug!("mDNS receive error: {e}");
                break;
            }
            Err(_) => break, // timeout
        }
    }
    let _ = daemon.stop_browse(SERVICE_TYPE);
    let _ = daemon.shutdown();
    let mut list: Vec<Device> = found.into_values().collect();
    list.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    Ok(list)
}

/// Parse `--to` target: device name, `ip`, or `ip:port`.
pub fn parse_target(s: &str, default_port: u16) -> Option<(IpAddr, u16)> {
    if let Ok(sa) = s.parse::<std::net::SocketAddr>() {
        return Some((sa.ip(), sa.port()));
    }
    if let Ok(ip) = s.parse::<IpAddr>() {
        return Some((ip, default_port));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_targets() {
        assert_eq!(parse_target("192.168.1.5", 47820), Some(("192.168.1.5".parse().unwrap(), 47820)));
        assert_eq!(parse_target("192.168.1.5:5000", 1), Some(("192.168.1.5".parse().unwrap(), 5000)));
        assert_eq!(parse_target("[::1]:9", 1), Some(("::1".parse().unwrap(), 9)));
        assert_eq!(parse_target("PC-Sala", 1), None);
    }

    #[test]
    fn instance_sanitised() {
        assert_eq!(sanitize_instance("my.pc"), "mypc");
        assert_eq!(sanitize_instance(""), "uni-share");
    }

    #[test]
    fn best_addr_prefers_v4() {
        let d = Device {
            name: "x".into(),
            addresses: vec!["fe80::1".parse().unwrap(), "10.0.0.2".parse().unwrap()],
            port: 1,
            fingerprint: String::new(),
            version: String::new(),
            requires_pin: false,
            online: true,
        };
        assert_eq!(d.best_addr(), Some("10.0.0.2".parse().unwrap()));
    }
}
