use crate::error::AppError;
use crate::net::ip::{is_valid_ipv4, sanitize_ipv4};
use crate::service::cache::TtlCache;
use std::time::Duration;
use tracing::info;

/// DNS cache TTL: 15 minutes (mirrors network.js cache behavior).
const DNS_CACHE_TTL: Duration = Duration::from_secs(15 * 60);

/// Resolve a domain to an IPv4 address with caching.
/// If the input is already an IPv4 address, returns it as-is.
/// Mirrors `resolve_domain_to_ip` from `modules/networking/network.js`.
pub async fn resolve_domain_to_ip(url_or_domain: &str, cache: &TtlCache) -> Result<String, AppError> {
    // Extract hostname from URL if needed
    let domain = extract_hostname(url_or_domain);

    // Already an IP? Return as-is.
    if is_valid_ipv4(&domain) {
        return Ok(sanitize_ipv4(&domain));
    }

    // Check cache
    let cache_key = format!("resolved_domain_{}", domain);
    if let Some(cached) = cache.get_string(&cache_key) {
        return Ok(cached);
    }

    // Resolve via DNS
    info!("Resolving domain: {}", domain);
    let resolved = tokio::net::lookup_host(format!("{}:0", domain))
        .await
        .map_err(|e| AppError::Internal(format!("DNS resolution failed for {}: {}", domain, e)))?
        .find(|addr| addr.is_ipv4())
        .map(|addr| addr.ip().to_string())
        .ok_or_else(|| AppError::Internal(format!("No IPv4 address found for {}", domain)))?;

    let sanitized = sanitize_ipv4(&resolved);
    info!("Resolved {} → {}", domain, sanitized);

    // Cache result
    cache.set_string(&cache_key, &sanitized, Some(DNS_CACHE_TTL));

    Ok(sanitized)
}

/// Extract hostname from a URL string.
/// "https://example.com:3000/path" → "example.com"
/// "example.com" → "example.com"
fn extract_hostname(input: &str) -> String {
    // Try parsing as URL
    if input.contains("://") {
        if let Some(after_scheme) = input.split("://").nth(1) {
            let host_port = after_scheme.split('/').next().unwrap_or(after_scheme);
            return host_port.split(':').next().unwrap_or(host_port).to_string();
        }
    }
    // Try stripping port
    input.split(':').next().unwrap_or(input).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_hostname() {
        assert_eq!(extract_hostname("https://example.com:3000/path"), "example.com");
        assert_eq!(extract_hostname("http://1.2.3.4:3000"), "1.2.3.4");
        assert_eq!(extract_hostname("example.com:3000"), "example.com");
        assert_eq!(extract_hostname("1.2.3.4"), "1.2.3.4");
    }
}
