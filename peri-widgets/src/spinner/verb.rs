use rand::RngExt;

/// 短中文名言，用于 loading spinner 随机展示。
pub const DEFAULT_VERBS: &[&str] = &[
    // ── 智慧/思考类 ──
    "格物致知",
    "见微知著",
    "大道至简",
    "慎思明辨",
    "融会贯通",
    "温故知新",
    "举一反三",
    // ── 行动/坚持类 ──
    "水滴石穿",
    "千里之行",
    "厚积薄发",
    "锲而不舍",
    "知行合一",
    "日拱一卒",
    "功不唐捐",
    "学以致用",
    // ── 创造/卓越类 ──
    "精益求精",
    "大巧若拙",
    "返璞归真",
    "独具匠心",
    "无中生有",
    // ── 心境/格局类 ──
    "上善若水",
    "海纳百川",
    "虚怀若谷",
    "心无旁骛",
    "宁静致远",
    "道法自然",
];

pub fn pick_verb(active_form: Option<&str>) -> String {
    active_form.map(|s| format!("{}…", s)).unwrap_or_else(|| {
        let mut rng = rand::rng();
        DEFAULT_VERBS[rng.random_range(0..DEFAULT_VERBS.len())].to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pick_verb_with_active_form() {
        let result = pick_verb(Some("搜索文件"));
        assert!(
            result.contains("搜索文件…"),
            "expected '搜索文件…', got '{}'",
            result
        );
    }

    #[test]
    fn test_pick_verb_random() {
        let result = pick_verb(None);
        assert!(!result.is_empty(), "verb should not be empty");
        assert!(
            DEFAULT_VERBS.contains(&result.as_str()),
            "'{}' should be in DEFAULT_VERBS",
            result
        );
    }
}
