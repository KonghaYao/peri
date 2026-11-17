use std::collections::HashSet;

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use crate::builtins::{is_builtin, is_reserved};

#[derive(Debug, Error)]
pub enum LibraryError {
    #[error("无效 JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("无效组件库: {0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
pub(crate) struct Schema {
    #[serde(rename = "type")]
    pub ty: Option<String>,
    pub properties: IndexMap<String, Schema>,
    pub required: Vec<String>,
    pub description: Option<String>,
    #[serde(rename = "$ref")]
    pub reference: Option<String>,
    #[serde(rename = "anyOf")]
    pub any_of: Vec<Schema>,
    #[serde(rename = "oneOf")]
    pub one_of: Vec<Schema>,
    #[serde(rename = "allOf")]
    pub all_of: Vec<Schema>,
    pub items: Option<Box<Schema>>,
    #[serde(rename = "enum")]
    pub enum_values: Option<Vec<Value>>,
    #[serde(rename = "const", deserialize_with = "present_value")]
    pub const_value: Option<Value>,
    #[serde(deserialize_with = "present_value")]
    pub default: Option<Value>,
}

fn present_value<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> std::result::Result<Option<Value>, D::Error> {
    Value::deserialize(d).map(Some)
}

impl Schema {
    pub(crate) fn composite(&self) -> bool {
        self.reference.is_some()
            || !self.any_of.is_empty()
            || !self.one_of.is_empty()
            || !self.all_of.is_empty()
    }
    pub(crate) fn ref_name(&self) -> Option<&str> {
        self.reference.as_deref()?.rsplit('/').next()
    }
    pub(crate) fn slot_names(&self) -> Vec<Option<&str>> {
        let choices = if !self.any_of.is_empty() {
            &self.any_of
        } else {
            &self.one_of
        };
        if choices.is_empty() {
            vec![self.ref_name()]
        } else {
            choices.iter().map(Self::ref_name).collect()
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct Param {
    pub name: String,
    pub required: bool,
    pub schema: Schema,
    pub binding: bool,
}

#[derive(Debug, Clone)]
pub(crate) struct Component {
    pub params: Vec<Param>,
    pub description: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CallDef {
    pub params: Schema,
    pub returns: Option<Schema>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct SchemaDocument {
    #[serde(rename = "$defs")]
    defs: IndexMap<String, Schema>,
    properties: Option<IndexMap<String, Value>>,
    functions: IndexMap<String, CallDef>,
    actions: IndexMap<String, CallDef>,
}

#[derive(Debug, Clone, Default, Deserialize)]
#[serde(default)]
struct PromptComponent {
    signature: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
enum Document {
    Spec {
        schema: SchemaDocument,
        root: Option<String>,
        #[serde(default)]
        components: IndexMap<String, PromptComponent>,
    },
    Bare(SchemaDocument),
}

#[derive(Debug, Clone)]
pub struct Library {
    pub(crate) root: Option<String>,
    pub(crate) components: IndexMap<String, Component>,
    pub(crate) functions: IndexMap<String, CallDef>,
    pub(crate) actions: IndexMap<String, CallDef>,
}

impl Library {
    pub fn from_json_str(input: &str) -> std::result::Result<Self, LibraryError> {
        Self::compile(serde_json::from_str(input)?, None)
    }

    /// Value 已失去的原始 key 顺序无法恢复；需要位置参数顺序时使用 from_json_str。
    pub fn from_value(
        value: Value,
        root: Option<String>,
    ) -> std::result::Result<Self, LibraryError> {
        Self::compile(serde_json::from_value(value)?, root)
    }

    pub fn root(&self) -> Option<&str> {
        self.root.as_deref()
    }

    fn compile(
        document: Document,
        root: Option<String>,
    ) -> std::result::Result<Self, LibraryError> {
        let (schema, declared_root, prompt_components) = match document {
            Document::Spec {
                schema,
                root,
                components,
            } => (schema, root, components),
            Document::Bare(schema) => (schema, None, IndexMap::new()),
        };
        if schema.defs.is_empty() {
            return Err(LibraryError::Invalid("缺少非空 $defs".into()));
        }
        let mut action_defs = IndexMap::new();
        for name in ["ActionExpression", "OpenUrl", "ToAssistant"]
            .into_iter()
            .chain(schema.actions.keys().map(String::as_str))
        {
            if !schema
                .properties
                .as_ref()
                .is_some_and(|p| p.contains_key(name))
            {
                if let Some(def) = schema.defs.get(name) {
                    action_defs.insert(name.to_owned(), def.clone());
                }
            }
        }
        let mut expansions = 0;
        let mut components = IndexMap::new();
        for (name, def) in &schema.defs {
            let component = schema
                .properties
                .as_ref()
                .map_or(!action_defs.contains_key(name), |p| p.contains_key(name));
            if !component {
                continue;
            }
            if is_builtin(name) || is_reserved(name) {
                return Err(LibraryError::Invalid(format!("组件 {name} 与保留名称冲突")));
            }
            let signature = prompt_components
                .get(name)
                .map(|p| p.signature.as_str())
                .unwrap_or("");
            let params = def
                .properties
                .iter()
                .map(|(key, property)| {
                    let mut property = property.clone();
                    inline_actions(
                        &mut property,
                        &action_defs,
                        &mut HashSet::new(),
                        &mut expansions,
                    )?;
                    Ok(Param {
                        name: key.clone(),
                        required: def.required.contains(key),
                        schema: property,
                        binding: signature.split([',', '(']).any(|part| {
                            part.trim_start().starts_with(&format!("{key}: $binding<"))
                                || part.trim_start().starts_with(&format!("{key}?: $binding<"))
                        }),
                    })
                })
                .collect::<std::result::Result<Vec<_>, LibraryError>>()?;
            components.insert(
                name.clone(),
                Component {
                    params,
                    description: def.description.clone().unwrap_or_default(),
                },
            );
        }
        let root = root.or(declared_root);
        if let Some(root) = &root {
            if !components.contains_key(root) {
                return Err(LibraryError::Invalid(format!("root 组件 {root} 不存在")));
            }
        }
        let mut names: HashSet<String> = components.keys().cloned().collect();
        for name in schema.functions.keys().chain(schema.actions.keys()) {
            if is_builtin(name) || is_reserved(name) || !names.insert(name.clone()) {
                return Err(LibraryError::Invalid(format!("调用 {name} 名称冲突")));
            }
        }
        let mut library = Self {
            root,
            components,
            functions: schema.functions,
            actions: schema.actions,
        };
        for def in library
            .functions
            .values_mut()
            .chain(library.actions.values_mut())
        {
            inline_actions(
                &mut def.params,
                &action_defs,
                &mut HashSet::new(),
                &mut expansions,
            )?;
            if let Some(returns) = &mut def.returns {
                inline_actions(returns, &action_defs, &mut HashSet::new(), &mut expansions)?;
            }
        }
        Ok(library)
    }
}

fn inline_actions(
    schema: &mut Schema,
    defs: &IndexMap<String, Schema>,
    seen: &mut HashSet<String>,
    expansions: &mut usize,
) -> std::result::Result<(), LibraryError> {
    if let Some(name) = schema.ref_name().map(str::to_owned) {
        if let Some(def) = defs.get(&name) {
            if seen.len() >= crate::MAX_DEPTH || !seen.insert(name.clone()) {
                return Err(LibraryError::Invalid(format!(
                    "action schema 循环或过深引用 {name}"
                )));
            }
            if *expansions >= crate::MAX_SCHEMA_EXPANSIONS {
                return Err(LibraryError::Invalid(
                    "action schema 展开超过安全预算".into(),
                ));
            }
            *expansions += 1;
            *schema = def.clone();
            inline_actions(schema, defs, seen, expansions)?;
            seen.remove(&name);
            return Ok(());
        }
    }
    for child in schema
        .properties
        .values_mut()
        .chain(schema.any_of.iter_mut())
        .chain(schema.one_of.iter_mut())
        .chain(schema.all_of.iter_mut())
    {
        inline_actions(child, defs, seen, expansions)?;
    }
    if let Some(items) = &mut schema.items {
        inline_actions(items, defs, seen, expansions)?;
    }
    Ok(())
}
