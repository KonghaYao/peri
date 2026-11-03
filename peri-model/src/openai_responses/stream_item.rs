use std::collections::{BTreeMap, BTreeSet};

use serde_json::Value;

use crate::{ModelResult, ModelStreamEvent};

use super::{
    request::invalid,
    response::{function_name, required_array, required_string, required_string_allow_empty},
    ResponsesProfile,
};

pub(super) struct StreamItem {
    identity: Value,
    pub index: usize,
    kind: String,
    parts: BTreeMap<usize, (String, String)>,
    arguments: String,
    done: Option<Value>,
}

impl StreamItem {
    pub(super) fn new(
        index: usize,
        item: &Value,
        profile: ResponsesProfile,
        tools: &BTreeSet<String>,
    ) -> ModelResult<(Self, Vec<ModelStreamEvent>)> {
        if item.get("id").is_some() {
            required_string(item, "id")?;
        }
        let kind = required_string(item, "type")?.to_owned();
        let mut events = Vec::new();
        match kind.as_str() {
            "message" => {
                if required_string(item, "role")? != "assistant" {
                    return Err(invalid("Output message has a non-assistant role"));
                }
            }
            "reasoning" => {}
            "function_call" => {
                let name = function_name(item, profile)?;
                if !tools.contains(name) {
                    return Err(invalid("Responses called a function that was not offered"));
                }
                events.push(ModelStreamEvent::ToolCallDelta {
                    index,
                    id: Some(required_string(item, "call_id")?.to_owned()),
                    name: Some(name.to_owned()),
                    arguments_delta: String::new(),
                });
            }
            _ => {
                return Err(invalid(
                    "Unsupported Responses output item; hosted/executable tools are not enabled",
                ));
            }
        }
        let mut state = Self {
            identity: item.clone(),
            index,
            kind,
            parts: BTreeMap::new(),
            arguments: String::new(),
            done: None,
        };
        events.extend(state.reconcile(item)?);
        Ok((state, events))
    }

    pub(super) fn id(&self) -> Option<&str> {
        self.identity["id"].as_str()
    }

    pub(super) fn call_id(&self) -> Option<&str> {
        if self.kind == "function_call" {
            self.identity["call_id"].as_str()
        } else {
            None
        }
    }

    pub(super) fn finished_snapshot(&self) -> Option<&Value> {
        self.done.as_ref()
    }

    pub(super) fn truncate(&mut self, item: &Value) -> ModelResult<Vec<ModelStreamEvent>> {
        match item.get("status").filter(|status| !status.is_null()) {
            Some(status) if status == "completed" => self.finish(item),
            Some(status) if status != "in_progress" && status != "incomplete" => {
                Err(invalid("Responses output item has an invalid status"))
            }
            _ if self.done.is_some() => self.finish(item),
            _ => self.reconcile(item),
        }
    }

    pub(super) fn truncated_snapshot(&self) -> ModelResult<Option<Value>> {
        if let Some(done) = &self.done {
            match done.get("status").filter(|status| !status.is_null()) {
                None => return Ok(Some(done.clone())),
                Some(status) if status == "completed" => return Ok(Some(done.clone())),
                Some(status) if status == "in_progress" || status == "incomplete" => {}
                _ => return Err(invalid("Responses output item has an invalid status")),
            }
        }
        if self.done.is_none()
            && self
                .identity
                .get("status")
                .filter(|status| !status.is_null())
                .is_some_and(|status| {
                    !matches!(
                        status.as_str(),
                        Some("completed" | "in_progress" | "incomplete")
                    )
                })
        {
            return Err(invalid("Responses output item has an invalid status"));
        }
        // 半截思考和函数调用没有安全回放依据；正文只保留已校验的纯内容，
        // 不伪造原生 item 的完成状态或携带其未完成身份。
        if self.kind != "message" || self.parts.values().all(|(_, text)| text.is_empty()) {
            return Ok(None);
        }
        let content: Vec<Value> = self
            .parts
            .values()
            .map(|(kind, text)| {
                if kind == "refusal" {
                    serde_json::json!({"type": kind, "refusal": text})
                } else {
                    serde_json::json!({"type": kind, "text": text})
                }
            })
            .collect();
        Ok(Some(serde_json::json!({
            "type": "message", "role": "assistant", "content": content
        })))
    }

    pub(super) fn check_id(&self, event: &Value) -> ModelResult<()> {
        if Some(required_string(event, "item_id")?) != self.id() {
            return Err(invalid("Responses output_index and item_id disagree"));
        }
        Ok(())
    }

    pub(super) fn part_delta(
        &mut self,
        index: usize,
        part_kind: &str,
        delta: &str,
    ) -> ModelResult<Vec<ModelStreamEvent>> {
        self.ensure_live()?;
        let valid = match self.kind.as_str() {
            "message" => matches!(part_kind, "output_text" | "refusal"),
            "reasoning" => part_kind == "summary_text",
            _ => false,
        };
        if !valid {
            return Err(invalid(
                "Responses delta kind does not match its output item",
            ));
        }
        let part = self
            .parts
            .entry(index)
            .or_insert_with(|| (part_kind.to_owned(), String::new()));
        if part.0 != part_kind {
            return Err(invalid("Responses content index changed its content kind"));
        }
        part.1.push_str(delta);
        Ok(self.text_event(delta))
    }

    pub(super) fn arguments_delta(&mut self, delta: &str) -> ModelResult<Vec<ModelStreamEvent>> {
        self.ensure_live()?;
        if self.kind != "function_call" {
            return Err(invalid("Arguments delta belongs to a non-function item"));
        }
        self.arguments.push_str(delta);
        Ok(self.arguments_event(delta))
    }

    pub(super) fn arguments_done(&mut self, arguments: &str) -> ModelResult<Vec<ModelStreamEvent>> {
        self.ensure_live()?;
        if self.kind != "function_call" {
            return Err(invalid(
                "Arguments completion belongs to a non-function item",
            ));
        }
        let suffix = arguments
            .strip_prefix(&self.arguments)
            .ok_or_else(|| invalid("Function arguments snapshot disagrees with emitted deltas"))?
            .to_owned();
        self.arguments = arguments.to_owned();
        Ok(self.arguments_event(&suffix))
    }

    pub(super) fn part_done(
        &mut self,
        index: usize,
        kind: &str,
        text: &str,
    ) -> ModelResult<Vec<ModelStreamEvent>> {
        self.ensure_live()?;
        let emitted = self
            .parts
            .get(&index)
            .map(|part| part.1.as_str())
            .unwrap_or("");
        let suffix = text
            .strip_prefix(emitted)
            .ok_or_else(|| invalid("Content snapshot disagrees with emitted deltas"))?
            .to_owned();
        self.part_delta(index, kind, &suffix)
    }

    pub(super) fn finish(&mut self, item: &Value) -> ModelResult<Vec<ModelStreamEvent>> {
        if let Some(done) = &self.done {
            if equivalent_finished_item(done, item) {
                return Ok(Vec::new());
            }
            return Err(invalid(
                "Completed Responses item changed after output_item.done",
            ));
        }
        let events = self.reconcile(item)?;
        self.done = Some(item.clone());
        Ok(events)
    }

    fn reconcile(&mut self, item: &Value) -> ModelResult<Vec<ModelStreamEvent>> {
        for field in ["id", "type", "role", "call_id", "name", "namespace"] {
            // Optional fields may be omitted initially and serialized as null
            // later. Both represent the same absent namespace/identity field.
            let before = self.identity.get(field).filter(|value| !value.is_null());
            let after = item.get(field).filter(|value| !value.is_null());
            if before != after {
                return Err(invalid("Responses item changed identity during the stream"));
            }
        }
        if self.kind == "function_call" {
            return self.arguments_done(required_string_allow_empty(item, "arguments")?);
        }
        let field = if self.kind == "message" {
            "content"
        } else {
            "summary"
        };
        let parts = required_array(item, field)?;
        if self.parts.keys().any(|index| *index >= parts.len()) {
            return Err(invalid(
                "Responses snapshot omitted previously emitted content",
            ));
        }
        let mut events = Vec::new();
        for (index, part) in parts.iter().enumerate() {
            let kind = required_string(part, "type")?;
            let text = required_string_allow_empty(
                part,
                if kind == "refusal" { "refusal" } else { "text" },
            )?;
            events.extend(self.part_done(index, kind, text)?);
        }
        Ok(events)
    }

    fn ensure_live(&self) -> ModelResult<()> {
        if self.done.is_some() {
            return Err(invalid("Responses delta arrived after output_item.done"));
        }
        Ok(())
    }

    fn text_event(&self, text: &str) -> Vec<ModelStreamEvent> {
        if text.is_empty() {
            return Vec::new();
        }
        vec![if self.kind == "reasoning" {
            ModelStreamEvent::ReasoningDelta {
                text: text.to_owned(),
            }
        } else {
            ModelStreamEvent::TextDelta {
                text: text.to_owned(),
            }
        }]
    }

    fn arguments_event(&self, delta: &str) -> Vec<ModelStreamEvent> {
        if delta.is_empty() {
            return Vec::new();
        }
        vec![ModelStreamEvent::ToolCallDelta {
            index: self.index,
            id: None,
            name: None,
            arguments_delta: delta.to_owned(),
        }]
    }
}

fn equivalent_finished_item(before: &Value, after: &Value) -> bool {
    let normalize = |value: &Value| {
        let mut value = value.clone();
        if value.get("namespace").is_some_and(Value::is_null) {
            value
                .as_object_mut()
                .expect("validated object")
                .remove("namespace");
        }
        value
    };
    normalize(before) == normalize(after)
}
