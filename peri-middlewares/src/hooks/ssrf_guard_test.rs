use super::Ipv6Addr;

#[test]
fn test_is_blocked_ipv4_private() {
    assert!(super::is_blocked_ipv4("10.0.0.1".parse().unwrap()));
    assert!(super::is_blocked_ipv4("192.168.1.1".parse().unwrap()));
    assert!(super::is_blocked_ipv4("172.16.0.1".parse().unwrap()));
}

#[test]
fn test_is_blocked_ipv4_metadata() {
    assert!(super::is_blocked_ipv4("169.254.169.254".parse().unwrap()));
    assert!(super::is_blocked_ipv4("100.100.100.200".parse().unwrap()));
}

#[test]
fn test_is_blocked_ipv4_loopback_allowed() {
    assert!(!super::is_blocked_ipv4("127.0.0.1".parse().unwrap()));
}

#[test]
fn test_is_blocked_ipv4_this_network() {
    assert!(super::is_blocked_ipv4("0.0.0.1".parse().unwrap()));
}

#[test]
fn test_is_blocked_ipv6_private() {
    assert!(super::is_blocked_ipv6("fc00::1".parse().unwrap()));
    assert!(super::is_blocked_ipv6("fe80::1".parse().unwrap()));
}

#[test]
fn test_is_blocked_ipv6_loopback_allowed() {
    assert!(!super::is_blocked_ipv6("::1".parse().unwrap()));
}

#[test]
fn test_is_blocked_ipv6_unspecified() {
    assert!(super::is_blocked_ipv6("::".parse().unwrap()));
}

#[test]
fn test_is_blocked_ipv4_mapped_ipv6() {
    // ::ffff:192.168.1.1 should be blocked
    let ip: Ipv6Addr = "::ffff:192.168.1.1".parse().unwrap();
    assert!(super::is_blocked_ipv6(ip));
}

#[test]
fn test_is_blocked_ipv4_mapped_loopback_allowed() {
    // ::ffff:127.0.0.1 should be allowed (loopback)
    let ip: Ipv6Addr = "::ffff:127.0.0.1".parse().unwrap();
    assert!(!super::is_blocked_ipv6(ip));
}

#[test]
fn test_check_url_invalid_url() {
    assert!(super::check_url("not-a-url").is_err());
}

#[test]
fn test_check_url_loopback_allowed() {
    // 127.0.0.1 should be allowed
    let result = super::check_url("http://127.0.0.1:8080");
    assert!(result.is_ok());
}

#[test]
fn test_check_url_private_blocked() {
    // 192.168.x.x should be blocked
    let result = super::check_url("http://192.168.1.1");
    assert!(result.is_err());
}

#[test]
fn test_check_url_metadata_blocked() {
    // 169.254.169.254 should be blocked
    let result = super::check_url("http://169.254.169.254/latest/meta-data/");
    assert!(result.is_err());
}

#[test]
fn test_check_url_ipv6_loopback_allowed() {
    let result = super::check_url("http://[::1]:8080");
    assert!(result.is_ok());
}

#[test]
fn test_check_url_ipv6_private_blocked() {
    let result = super::check_url("http://[fc00::1]");
    assert!(result.is_err());
}

#[test]
fn test_is_blocked_ipv6_public_allowed() {
    // ::/0 removed: 公网 IPv6 不再被阻止
    // 2001:db8::/32 是 IETF 文档保留前缀，不在任何阻止范围内
    assert!(!super::is_blocked_ipv6("2001:db8::1".parse().unwrap()));
    // Google DNS IPv6 (公网)
    assert!(!super::is_blocked_ipv6(
        "2001:4860:4860::8888".parse().unwrap()
    ));
    // Cloudflare DNS IPv6 (公网)
    assert!(!super::is_blocked_ipv6(
        "2606:4700:4700::1111".parse().unwrap()
    ));
}

#[test]
fn test_check_url_ipv4_mapped_blocked() {
    let result = super::check_url("http://[::ffff:192.168.1.1]");
    assert!(result.is_err());
}
