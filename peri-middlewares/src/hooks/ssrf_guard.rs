use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

/// SSRF 防护：阻止对私有/内部网络的 HTTP 请求
///
/// 阻止范围（IPv4）：
///   0.0.0.0/8        "this" network
///   10.0.0.0/8       private
///   100.64.0.0/10    CGNAT / shared address space（部分云 metadata）
///   169.254.0.0/16   link-local（云 metadata）
///   172.16.0.0/12    private
///   192.168.0.0/16   private
///
/// 阻止范围（IPv6）:
///   fc00::/7         unique local (ULA)
///   fe80::/10        link-local
///   ::ffff:<v4>      mapped IPv4 in blocked range
///
/// 允许（不阻止）：
///   127.0.0.0/8      loopback（本地开发 hook）
///   ::1              loopback
///   其他所有公网地址
pub fn check_url(url: &str) -> Result<(), String> {
    // 1. 解析 URL
    let parsed = reqwest::Url::parse(url).map_err(|_| "Invalid URL".to_string())?;

    // 2. 提取 host
    let host = parsed
        .host_str()
        .ok_or_else(|| "No host in URL".to_string())?;

    // 3. DNS 解析
    let port = parsed.port().unwrap_or(80);
    let addr_str = format!("{}:{}", host, port);

    let addrs: Vec<SocketAddr> = match std::net::ToSocketAddrs::to_socket_addrs(&addr_str) {
        Ok(iter) => iter.collect(),
        Err(_) => return Err("DNS resolution failed".to_string()),
    };

    if addrs.is_empty() {
        return Err("DNS resolution returned no addresses".to_string());
    }

    // 4. 检查每个 IP
    for addr in &addrs {
        match addr.ip() {
            IpAddr::V4(ip) => {
                if is_blocked_ipv4(ip) {
                    return Err(format!("Blocked: {}", ip));
                }
            }
            IpAddr::V6(ip) => {
                if is_blocked_ipv6(ip) {
                    return Err(format!("Blocked: {}", ip));
                }
            }
        }
    }

    Ok(())
}

/// 检查 IPv4 地址是否在阻止范围内
fn is_blocked_ipv4(ip: Ipv4Addr) -> bool {
    // 允许 loopback
    if ip.is_loopback() {
        return false;
    }

    use ipnet::Ipv4Net;
    let blocked_ranges: &[Ipv4Net] = &[
        "0.0.0.0/8"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // "this" network
        "10.0.0.0/8"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // private
        "100.64.0.0/10"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // CGNAT / shared address space
        "169.254.0.0/16"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // link-local（云 metadata）
        "172.16.0.0/12"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // private
        "192.168.0.0/16"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // private
    ];

    blocked_ranges.iter().any(|range| range.contains(&ip))
}

/// 检查 IPv6 地址是否在阻止范围内
fn is_blocked_ipv6(ip: Ipv6Addr) -> bool {
    // 允许 loopback
    if ip.is_loopback() {
        return false;
    }

    // 检查 IPv4-mapped IPv6 地址（::ffff:<v4>）
    if let Some(v4) = ip.to_ipv4_mapped() {
        return is_blocked_ipv4(v4);
    }

    use ipnet::Ipv6Net;
    let blocked_ranges: &[Ipv6Net] = &[
        "fc00::/7"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // unique local
        "fe80::/10"
            .parse()
            .expect("hardcoded SSRF CIDR range must be syntactically valid"), // link-local
    ];

    // :: (unspecified) — checked via is_unspecified()
    if ip.is_unspecified() {
        return true;
    }

    blocked_ranges.iter().any(|range| range.contains(&ip))
}

#[cfg(test)]
#[path = "ssrf_guard_test.rs"]
mod tests;
