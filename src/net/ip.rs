#![allow(dead_code)]
use std::net::{IpAddr, SocketAddr};

/// Extract the unspoofable (TCP-layer) IP from a socket address.
/// Mirrors `ip_from_req(request)` from `modules/networking/network.js`.
pub fn ip_from_socket(addr: &SocketAddr) -> String {
    let ip = addr.ip();
    sanitize_ipv4(&ip.to_string())
}

/// Normalize IPv4 addresses: strip ::ffff: prefix, convert ::1 to 127.0.0.1.
/// Mirrors the sanitization logic from network.js.
pub fn sanitize_ipv4(ip: &str) -> String {
    let trimmed = ip.trim();
    // Strip IPv4-mapped IPv6 prefix
    let stripped = trimmed.strip_prefix("::ffff:").unwrap_or(trimmed);
    // Convert IPv6 loopback to IPv4 loopback
    if stripped == "::1" {
        return "127.0.0.1".to_string();
    }
    stripped.to_string()
}

/// Check if an IP address is from the local/internal Docker network.
/// Mirrors `request_is_local(request)` from network.js.
pub fn is_local_ip(ip: &str, internal_subnet: &str, external_subnet: &str) -> bool {
    let sanitized = sanitize_ipv4(ip);

    // Localhost
    if sanitized == "127.0.0.1" || sanitized == "localhost" || sanitized == "::1" {
        return true;
    }

    // Check Docker subnets (prefix match on the network portion)
    // Internal: 172.20.x.x, External: 172.21.x.x
    let internal_prefix = subnet_prefix(internal_subnet);
    let external_prefix = subnet_prefix(external_subnet);

    sanitized.starts_with(&internal_prefix) || sanitized.starts_with(&external_prefix)
}

/// Extract the network prefix from a CIDR notation (e.g., "172.20.0.0/16" → "172.20.").
fn subnet_prefix(cidr: &str) -> String {
    let parts: Vec<&str> = cidr.split('/').collect();
    let ip = parts.first().unwrap_or(&"");
    let mask_bits: u32 = parts.get(1).and_then(|s| s.parse().ok()).unwrap_or(16);

    let octets: Vec<&str> = ip.split('.').collect();
    let significant_octets = (mask_bits / 8) as usize;

    let prefix: Vec<&str> = octets.iter().take(significant_octets).copied().collect();
    if prefix.is_empty() {
        String::new()
    } else {
        format!("{}.", prefix.join("."))
    }
}

/// Validate that a string looks like a valid IPv4 address.
pub fn is_valid_ipv4(ip: &str) -> bool {
    ip.parse::<IpAddr>()
        .map(|addr| addr.is_ipv4())
        .unwrap_or(false)
}
