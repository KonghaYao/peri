use std::collections::HashMap;

use fluent::FluentResource;
use fluent_bundle::{FluentArgs, FluentBundle, FluentValue};

const EN_FTL: &str = include_str!("../../locales/en/main.ftl");
const ZH_CN_FTL: &str = include_str!("../../locales/zh-CN/main.ftl");

pub struct LcRegistry {
    current_lang: String,
    bundles: HashMap<String, FluentBundle<FluentResource>>,
}

impl LcRegistry {
    pub fn new(lang: Option<&str>) -> Self {
        let mut bundles = HashMap::new();
        bundles.insert("en".to_string(), Self::create_bundle("en", EN_FTL));
        bundles.insert("zh-CN".to_string(), Self::create_bundle("zh-CN", ZH_CN_FTL));

        let current_lang = match lang {
            Some(l) if bundles.contains_key(l) => l.to_string(),
            Some(l) => {
                tracing::warn!("unsupported language '{}', falling back to 'en'", l);
                "en".to_string()
            }
            None => "en".to_string(),
        };

        Self {
            current_lang,
            bundles,
        }
    }

    fn create_bundle(lang: &str, source: &str) -> FluentBundle<FluentResource> {
        let langid = match lang {
            "en" => unic_langid::langid!("en"),
            "zh-CN" => unic_langid::langid!("zh-CN"),
            _ => unic_langid::langid!("en"),
        };
        let resource = FluentResource::try_new(source.to_string()).expect("FTL parse error");
        let mut bundle = FluentBundle::new(vec![langid]);
        bundle
            .add_resource(resource)
            .expect("Failed to add FTL resource");
        bundle.set_use_isolating(false);
        bundle
    }

    pub fn tr(&self, key: &str) -> String {
        self.format_key(key, None)
    }

    pub fn tr_args(&self, key: &str, args: &[(String, FluentValue<'_>)]) -> String {
        let mut fa = FluentArgs::new();
        for (k, v) in args {
            fa.set(k.as_str(), v.clone());
        }
        self.format_key(key, Some(&fa))
    }

    fn format_key(&self, key: &str, args: Option<&FluentArgs<'_>>) -> String {
        if let Some(value) = self.format_in_bundle(&self.current_lang, key, args) {
            return value;
        }
        if self.current_lang != "en" {
            if let Some(value) = self.format_in_bundle("en", key, args) {
                return value;
            }
        }
        key.to_string()
    }

    fn format_in_bundle(
        &self,
        lang: &str,
        key: &str,
        args: Option<&FluentArgs<'_>>,
    ) -> Option<String> {
        let bundle = self.bundles.get(lang)?;
        let msg = bundle.get_message(key)?;
        let pattern = msg.value()?;
        let mut errors = vec![];
        let value = bundle.format_pattern(pattern, args, &mut errors);
        if !errors.is_empty() {
            tracing::warn!("FTL format errors for key '{}': {:?}", key, errors);
        }
        Some(value.to_string())
    }

    pub fn switch(&mut self, lang: &str) -> anyhow::Result<()> {
        if self.bundles.contains_key(lang) {
            self.current_lang = lang.to_string();
            Ok(())
        } else {
            Err(anyhow::anyhow!("unsupported language: {}", lang))
        }
    }

    pub fn available_langs(&self) -> Vec<&str> {
        self.bundles.keys().map(|s| s.as_str()).collect()
    }

    pub fn current_lang(&self) -> &str {
        self.current_lang.as_str()
    }
}

impl Default for LcRegistry {
    fn default() -> Self {
        Self::new(None)
    }
}

#[cfg(test)]
mod tests {
    include!("mod_test.rs");
}
