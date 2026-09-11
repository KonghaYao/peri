use super::oauth_delivery::{oauth_delivery_policy, OAuthDeliveryPolicy};

#[test]
fn test_oauth_delivery_policy_keeps_safe_and_legacy_caps_independent() {
    let safe_only = peri_acp_types::PeriCaps {
        oauth: true,
        ..Default::default()
    };
    assert_eq!(
        oauth_delivery_policy(&safe_only),
        OAuthDeliveryPolicy {
            safe: true,
            legacy: false
        }
    );
    let legacy_only = peri_acp_types::PeriCaps {
        agent_event: true,
        ..Default::default()
    };
    assert_eq!(
        oauth_delivery_policy(&legacy_only),
        OAuthDeliveryPolicy {
            safe: false,
            legacy: true
        }
    );
}
