use peri_acp::prompt::{PromptEnv, PromptFeatures, PromptTemplate};
use peri_acp_types::meta_harness::MetaHarnessState;
use peri_agent::middleware::{PromptSection, PromptSectionZone};
use peri_middlewares::host_ports::SkillsProvider;
use peri_model::prompt_cache::SYSTEM_PROMPT_DYNAMIC_BOUNDARY;
use peri_model::{AnthropicConfig, AnthropicModel, Model, ModelMessage, ModelRequest};
use url::Url;

#[test]
fn prompt_template_to_anthropic_preserves_cache_seam_and_dynamic_order() {
    let sections = vec![
        PromptSection::dynamic(
            "cached-test",
            PromptSectionZone::Cached,
            1,
            "BASE-STATIC".into(),
        ),
        PromptSection::dynamic(
            "uncached-test",
            PromptSectionZone::Uncached,
            1,
            "BASE-DYNAMIC".into(),
        ),
    ];
    let rendered = PromptTemplate::new(&MetaHarnessState::default(), &sections).render(
        &PromptEnv::with_frozen_date("/tmp", "2026-01-01"),
        &PromptFeatures::detect(),
        &SkillsProvider,
        &[],
    );
    let request = ModelRequest::new(vec![
        ModelMessage::system_text(rendered),
        ModelMessage::system_text("REQUEST-MIDDLEWARE"),
        ModelMessage::user_text("go"),
    ]);
    let model = AnthropicModel::new(AnthropicConfig::new(
        Url::parse("https://proxy.example.test/").expect("valid endpoint"),
        "test-credential",
        "claude-test",
    ));
    let prepared = model.prepare_request(&request).expect("request prepares");
    let system = prepared.body().as_value()["system"]
        .as_array()
        .expect("Anthropic system blocks");

    assert_eq!(system[0]["text"], "BASE-STATIC");
    assert_eq!(system[0]["cache_control"]["type"], "ephemeral");
    assert_eq!(system[1]["text"], "BASE-DYNAMIC\n\nREQUEST-MIDDLEWARE");
    assert!(system[1].get("cache_control").is_none());
    assert!(!serde_json::to_string(system)
        .expect("system serializes")
        .contains(SYSTEM_PROMPT_DYNAMIC_BOUNDARY));
}
