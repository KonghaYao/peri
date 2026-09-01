//! Provider-neutral transport contract for preserving a system prompt cache seam.

/// Reserved zero-width transport token between cacheable and dynamic system sections.
///
/// Producers may place this token in the model-neutral system text. Provider adapters
/// must consume it before constructing their wire request.
pub const SYSTEM_PROMPT_DYNAMIC_BOUNDARY: &str = "__SYSTEM_PROMPT_DYNAMIC_BOUNDARY__";

/// Removes every reserved cache-boundary token without changing surrounding bytes.
pub fn strip_system_prompt_dynamic_boundaries(text: &str) -> String {
    text.replace(SYSTEM_PROMPT_DYNAMIC_BOUNDARY, "")
}

/// Combines an optional frozen system prompt with a request-time contribution.
///
/// A non-empty contribution is always placed after an explicit cache boundary.
/// Removing the reserved token therefore reproduces the bytes emitted by the
/// legacy `base + "\n\n" + dynamic` composition, including its empty/absent-base
/// edge cases.
pub fn combine_system_prompt_with_dynamic(base: Option<&str>, dynamic: &str) -> Option<String> {
    if dynamic.is_empty() {
        return base.map(str::to_owned);
    }

    match base {
        Some(base) if base.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count() == 1 => {
            Some(format!("{base}\n\n{dynamic}"))
        }
        Some(base) => Some(format!(
            "{base}{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\n{dynamic}"
        )),
        None => Some(format!("{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}{dynamic}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_preserves_legacy_wire_bytes_for_all_base_states() {
        for (base, dynamic, expected, wire) in [
            (
                Some("BASE"),
                "DYNAMIC",
                format!("BASE{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nDYNAMIC"),
                "BASE\n\nDYNAMIC",
            ),
            (
                Some(""),
                "DYNAMIC",
                format!("{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nDYNAMIC"),
                "\n\nDYNAMIC",
            ),
            (
                None,
                "DYNAMIC",
                format!("{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}DYNAMIC"),
                "DYNAMIC",
            ),
        ] {
            let combined = combine_system_prompt_with_dynamic(base, dynamic).unwrap();
            assert_eq!(combined, expected);
            assert_eq!(strip_system_prompt_dynamic_boundaries(&combined), wire);
        }
    }

    #[test]
    fn combine_reuses_single_boundary_and_duplicate_remains_fail_closed() {
        let single = format!("STATIC{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}\n\nBASE-DYNAMIC");
        let combined = combine_system_prompt_with_dynamic(Some(&single), "REQUEST").unwrap();
        assert_eq!(combined.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 1);
        assert_eq!(
            strip_system_prompt_dynamic_boundaries(&combined),
            "STATIC\n\nBASE-DYNAMIC\n\nREQUEST"
        );

        let duplicate =
            format!("A{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}B{SYSTEM_PROMPT_DYNAMIC_BOUNDARY}C");
        let combined = combine_system_prompt_with_dynamic(Some(&duplicate), "REQUEST").unwrap();
        assert_eq!(combined.matches(SYSTEM_PROMPT_DYNAMIC_BOUNDARY).count(), 3);
        assert_eq!(
            strip_system_prompt_dynamic_boundaries(&combined),
            "ABC\n\nREQUEST"
        );
    }

    #[test]
    fn empty_dynamic_preserves_optional_base() {
        assert_eq!(
            combine_system_prompt_with_dynamic(Some("BASE"), ""),
            Some("BASE".into())
        );
        assert_eq!(combine_system_prompt_with_dynamic(None, ""), None);
    }
}
