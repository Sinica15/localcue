//! Local network discovery and LAN-only access checks.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};

use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Iface {
    pub name: String,
    pub ip: Ipv4Addr,
    /// VPN / container / hypervisor adapter: listed, but never picked automatically.
    pub virtual_link: bool,
}

/// Private (RFC 1918) addresses: the only ones we serve the teleprompter on.
pub fn is_private_v4(ip: Ipv4Addr) -> bool {
    ip.is_private()
}

/// Virtual / tunnel interfaces that are never the Wi-Fi a phone is on.
fn is_virtual_name(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [
        "utun",
        "docker",
        "veth",
        "br-",
        "vmnet",
        "vboxnet",
        "virbr",
        "tailscale",
        "awdl",
        "llw",
        "wsl",
        "zt",
    ]
    .iter()
    .any(|p| n.starts_with(p))
        || n.contains("vethernet")
        || n.contains("virtual")
}

/// Whether an address may be offered at all.
pub fn is_candidate(ip: Ipv4Addr) -> bool {
    is_private_v4(ip) && !ip.is_loopback()
}

fn rank(ip: Ipv4Addr) -> u8 {
    match ip.octets() {
        [192, 168, ..] => 0,
        [10, ..] => 1,
        _ => 2,
    }
}

/// The IPv4 address the OS would use to reach the internet. UDP `connect`
/// sends no packets; it only selects a route.
fn default_route_ip() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(v4) => Some(v4),
        _ => None,
    }
}

/// Pure ordering: real adapters before virtual ones, the preferred route address
/// first, then 192.168/16, 10/8, 172.16/12.
pub fn sort_candidates(mut list: Vec<Iface>, preferred: Option<Ipv4Addr>) -> Vec<Iface> {
    list.sort_by_key(|i| {
        (
            i.virtual_link,
            Some(i.ip) != preferred,
            rank(i.ip),
            i.name.clone(),
        )
    });
    list.dedup_by_key(|i| i.ip);
    list
}

/// All usable LAN interfaces, best first.
pub fn list_interfaces() -> Vec<Iface> {
    let all = local_ip_address::list_afinet_netifas().unwrap_or_default();
    let list = all
        .into_iter()
        .filter_map(|(name, ip)| match ip {
            IpAddr::V4(v4) if is_candidate(v4) => {
                let virtual_link = is_virtual_name(&name);
                Some(Iface {
                    name,
                    ip: v4,
                    virtual_link,
                })
            }
            _ => None,
        })
        .collect();
    sort_candidates(list, default_route_ip())
}

/// The address to use when the user has not chosen one.
pub fn auto_pick(list: &[Iface]) -> Option<Ipv4Addr> {
    list.iter().find(|i| !i.virtual_link).map(|i| i.ip)
}

/// Whether a peer may talk to the server: loopback or a private/link-local address.
pub fn is_lan_peer(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.is_loopback() || v4.is_private() || v4.is_link_local(),
            None => v6.is_loopback(),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Ipv4Addr {
        s.parse().unwrap()
    }

    fn iface(name: &str, addr: &str) -> Iface {
        Iface {
            name: name.into(),
            ip: ip(addr),
            virtual_link: is_virtual_name(name),
        }
    }

    #[test]
    fn only_private_non_loopback_addresses_are_candidates() {
        assert!(is_candidate(ip("192.168.1.20")));
        assert!(is_candidate(ip("10.0.0.5")));
        assert!(is_candidate(ip("172.20.1.1")));
        assert!(!is_candidate(ip("8.8.8.8"))); // public
        assert!(!is_candidate(ip("127.0.0.1")));
        assert!(!is_candidate(ip("169.254.3.3"))); // link-local
    }

    #[test]
    fn virtual_adapters_are_flagged_and_never_auto_picked() {
        assert!(is_virtual_name("utun3"));
        assert!(is_virtual_name("docker0"));
        assert!(is_virtual_name("vEthernet (WSL)"));
        assert!(!is_virtual_name("en0"));
        assert!(!is_virtual_name("Wi-Fi"));
        let l = sort_candidates(
            vec![iface("utun3", "10.8.0.2"), iface("en0", "172.20.0.4")],
            None,
        );
        assert_eq!(l[0].name, "en0"); // real adapter beats a "nicer" address on a tunnel
        assert_eq!(auto_pick(&l), Some(ip("172.20.0.4")));
        assert_eq!(auto_pick(&[iface("utun3", "10.8.0.2")]), None);
    }

    #[test]
    fn candidates_are_ordered_route_first_then_192_168() {
        let l = vec![
            iface("a", "172.16.0.2"),
            iface("b", "10.0.0.2"),
            iface("c", "192.168.0.2"),
        ];
        let s = sort_candidates(l.clone(), None);
        assert_eq!(
            s.iter().map(|i| i.ip).collect::<Vec<_>>(),
            vec![ip("192.168.0.2"), ip("10.0.0.2"), ip("172.16.0.2")]
        );
        let s = sort_candidates(l, Some(ip("10.0.0.2")));
        assert_eq!(s[0].ip, ip("10.0.0.2"));
    }

    #[test]
    fn lan_peer_check_rejects_public_addresses() {
        assert!(is_lan_peer("127.0.0.1".parse().unwrap()));
        assert!(is_lan_peer("192.168.1.9".parse().unwrap()));
        assert!(is_lan_peer("10.1.2.3".parse().unwrap()));
        assert!(is_lan_peer("::1".parse().unwrap()));
        assert!(is_lan_peer("::ffff:192.168.1.9".parse().unwrap()));
        assert!(!is_lan_peer("8.8.8.8".parse().unwrap()));
        assert!(!is_lan_peer("203.0.113.7".parse().unwrap()));
        assert!(!is_lan_peer("2001:db8::1".parse().unwrap()));
    }
}
