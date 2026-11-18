use super::*;
use peri_acp_types::meta_harness::{ExecutionProfile, MetaHarnessState, SECTION_IDS};

#[test]
fn declarations_match_the_owner_contract_and_derived_profiles() {
    for profile in [
        ExecutionProfile::Main,
        ExecutionProfile::Child,
        ExecutionProfile::Workflow {
            broker: false,
            permission_mode: false,
        },
        ExecutionProfile::Workflow {
            broker: true,
            permission_mode: true,
        },
    ] {
        for disabled in std::iter::once(None).chain(ChainSlot::ALL.iter().copied().map(Some)) {
            let mut state = MetaHarnessState::default();
            if let Some(owner) = disabled {
                state.disabled_middlewares.insert(owner.name().into());
            }
            let plan = AssemblyPlan::new(&state.disabled_middlewares, profile);
            let actual = collect_prompt_sections(None, None, &plan);
            let expected = ChainSlot::ALL
                .iter()
                .filter(|owner| plan.declares_sections(**owner))
                .flat_map(|owner| owner.sections());
            for spec in expected {
                assert!(actual.iter().any(|s| s.owner == Some(spec.owner)
                    && (s.id, s.zone, s.order) == (spec.id, spec.zone, spec.order)));
            }
            assert!(actual
                .iter()
                .all(|s| SECTION_IDS.contains(&s.id) && plan.declares_sections(s.owner.unwrap())));
        }
    }
}
