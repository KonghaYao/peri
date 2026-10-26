use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
};

use serde_json::Value;

use crate::{
    runtime::stream::{SseDecoderFactory, SseDecoders},
    transport::SseEvent,
    ModelError, ModelResult, ModelStreamEvent, ProtocolErrorKind,
};

use super::{
    replay::ReplayBinding,
    request::invalid,
    response::{self, required_array, required_string, required_string_allow_empty},
    stream_item::StreamItem,
};

pub(super) fn decoders(binding: ReplayBinding, tools: BTreeSet<String>) -> SseDecoderFactory {
    Arc::new(move || {
        let state = Arc::new(Mutex::new(ResponsesStream {
            binding: binding.clone(),
            tools: tools.clone(),
            items: BTreeMap::new(),
            response_id: None,
            completed: false,
        }));
        let decode_state = Arc::clone(&state);
        let decode = Arc::new(move |event: SseEvent, request_id: Option<String>| {
            decode_state
                .lock()
                .map_err(|_| invalid("Responses stream state lock poisoned"))?
                .decode(event, request_id)
        });
        let complete = Arc::new(move || {
            let state = state
                .lock()
                .map_err(|_| invalid("Responses stream state lock poisoned"))?;
            if state.completed {
                Ok(Vec::new())
            } else {
                Err(ModelError::protocol_with_summary(
                    ProtocolErrorKind::StreamEndedWithoutCompleted,
                    "Responses stream ended without response.completed",
                ))
            }
        });
        let decoders: SseDecoders = (decode, complete);
        decoders
    })
}

struct ResponsesStream {
    binding: ReplayBinding,
    tools: BTreeSet<String>,
    items: BTreeMap<usize, StreamItem>,
    response_id: Option<String>,
    completed: bool,
}

impl ResponsesStream {
    fn decode(
        &mut self,
        event: SseEvent,
        request_id: Option<String>,
    ) -> ModelResult<Vec<ModelStreamEvent>> {
        let value: Value = serde_json::from_str(&event.data).map_err(|error| {
            invalid("Malformed Responses SSE JSON")
                .with_error(&error)
                .with_body(&event.data)
        })?;
        let kind = required_string(&value, "type")?;
        if event
            .event
            .as_deref()
            .is_some_and(|name| !name.is_empty() && name != kind && name != "message")
        {
            return Err(invalid("Responses SSE event and JSON type disagree"));
        }
        if self.completed {
            return Err(invalid("Responses event arrived after response.completed"));
        }
        match kind {
            "response.created" | "response.in_progress" => {
                self.set_response_id(&value["response"])?;
                Ok(Vec::new())
            }
            "response.metadata" | "codex.response.metadata" => {
                if !value["response"].is_null() {
                    self.set_response_id(&value["response"])?;
                }
                Ok(Vec::new())
            }
            "response.output_item.added" => {
                self.add_item(index(&value, "output_index")?, &value["item"])
            }
            "response.output_item.done" => {
                let item_id = value["item"]
                    .get("id")
                    .map(|_| required_string(&value["item"], "id"))
                    .transpose()?;
                let index = match value.get("output_index") {
                    Some(_) => index(&value, "output_index")?,
                    None => self
                        .items
                        .iter()
                        .find(|(_, item)| item_id.is_some() && item.id() == item_id)
                        .map(|(&index, _)| index)
                        .unwrap_or_else(|| {
                            self.items.keys().next_back().map_or(0, |index| index + 1)
                        }),
                };
                let mut events = if self.items.contains_key(&index) {
                    Vec::new()
                } else {
                    self.add_item(index, &value["item"])?
                };
                events.extend(
                    self.items
                        .get_mut(&index)
                        .expect("item checked")
                        .finish(&value["item"])?,
                );
                Ok(events)
            }
            "response.output_text.delta"
            | "response.refusal.delta"
            | "response.reasoning_summary_text.delta" => {
                let part_kind = match kind {
                    "response.output_text.delta" => "output_text",
                    "response.refusal.delta" => "refusal",
                    _ => "summary_text",
                };
                let part_index = index(
                    &value,
                    if part_kind == "summary_text" {
                        "summary_index"
                    } else {
                        "content_index"
                    },
                )?;
                self.item(&value)?.part_delta(
                    part_index,
                    part_kind,
                    required_string_allow_empty(&value, "delta")?,
                )
            }
            "response.output_text.done"
            | "response.refusal.done"
            | "response.reasoning_summary_text.done" => {
                let part_kind = match kind {
                    "response.output_text.done" => "output_text",
                    "response.refusal.done" => "refusal",
                    _ => "summary_text",
                };
                let part_index = index(
                    &value,
                    if part_kind == "summary_text" {
                        "summary_index"
                    } else {
                        "content_index"
                    },
                )?;
                let field = if part_kind == "refusal" {
                    "refusal"
                } else {
                    "text"
                };
                self.item(&value)?.part_done(
                    part_index,
                    part_kind,
                    required_string_allow_empty(&value, field)?,
                )
            }
            "response.function_call_arguments.delta" => self
                .item(&value)?
                .arguments_delta(required_string_allow_empty(&value, "delta")?),
            "response.function_call_arguments.done" => self
                .item(&value)?
                .arguments_done(required_string_allow_empty(&value, "arguments")?),
            "response.content_part.added"
            | "response.content_part.done"
            | "response.reasoning_summary_part.added"
            | "response.reasoning_summary_part.done" => {
                // The item snapshot is authoritative; identity is still checked
                // on every lifecycle event so cross-item data cannot be mixed.
                self.item(&value)?;
                Ok(Vec::new())
            }
            "response.completed" => self.finish(&value["response"], request_id),
            "response.failed" | "response.incomplete" | "error" => {
                Err(invalid(&format!("OpenAI Responses terminal event: {kind}"))
                    .with_body(event.data))
            }
            // Output annotations do not change the normalized text. Their full
            // content remains in the completed native item for exact replay.
            "response.output_text.annotation.added" => {
                self.item(&value)?;
                Ok(Vec::new())
            }
            _ if value.get("item").is_some()
                || value.get("output_index").is_some()
                || kind.contains("tool")
                || kind.contains("call")
                || kind.starts_with("response.output_")
                || kind.starts_with("response.function_call_")
                || kind.starts_with("response.reasoning_")
                || kind.starts_with("response.custom_tool_") =>
            {
                Err(invalid(&format!(
                    "Unsupported Responses SSE output event: {kind}"
                )))
            }
            _ => Ok(Vec::new()),
        }
    }

    fn set_response_id(&mut self, response: &Value) -> ModelResult<()> {
        let id = required_string(response, "id")?;
        if self
            .response_id
            .as_deref()
            .is_some_and(|previous| previous != id)
        {
            return Err(invalid("Responses stream changed response ID"));
        }
        self.response_id = Some(id.to_owned());
        Ok(())
    }

    fn add_item(&mut self, index: usize, value: &Value) -> ModelResult<Vec<ModelStreamEvent>> {
        if index >= 100_000 {
            return Err(invalid(
                "Responses output_index exceeds the supported bound",
            ));
        }
        if self.items.contains_key(&index) {
            return Err(invalid("Responses output_index was added twice"));
        }
        let id = value
            .get("id")
            .map(|_| required_string(value, "id"))
            .transpose()?;
        if id.is_some() && self.items.values().any(|item| item.id() == id) {
            return Err(invalid("Responses item ID belongs to two output indices"));
        }
        let (item, events) = StreamItem::new(index, value, self.binding.profile, &self.tools)?;
        self.items.insert(index, item);
        Ok(events)
    }

    fn item(&mut self, event: &Value) -> ModelResult<&mut StreamItem> {
        let index = index(event, "output_index")?;
        let item = self
            .items
            .get_mut(&index)
            .ok_or_else(|| invalid("Responses delta has no matching output item"))?;
        item.check_id(event)?;
        Ok(item)
    }

    fn finish(
        &mut self,
        response: &Value,
        request_id: Option<String>,
    ) -> ModelResult<Vec<ModelStreamEvent>> {
        self.set_response_id(response)?;
        let output = response
            .get("output")
            .map(|_| required_array(response, "output"))
            .transpose()?;
        let missing_output = output.is_none();
        let empty = Vec::new();
        let output = output.unwrap_or(&empty);
        let compacted = missing_output || self.items.keys().any(|index| *index >= output.len());
        let mut recovered = None;
        if compacted {
            let mut items = Vec::with_capacity(self.items.len());
            let mut supplied = BTreeSet::new();
            for value in output {
                let id = required_string(value, "id")?;
                let Some((&index, _)) = self.items.iter().find(|(_, item)| item.id() == Some(id))
                else {
                    return Err(invalid(
                        "response.completed mixed omitted and untracked output items",
                    ));
                };
                if !supplied.insert(index) {
                    return Err(invalid("response.completed repeated an output item"));
                }
            }
            for (&index, item) in &self.items {
                let value = if supplied.contains(&index) {
                    output
                        .iter()
                        .find(|value| value["id"].as_str() == item.id())
                        .expect("matched item")
                } else {
                    item.finished_snapshot().ok_or_else(|| {
                        invalid("response.completed omitted an unfinished output item")
                    })?
                };
                items.push(value.clone());
            }
            let mut snapshot = response.clone();
            snapshot
                .as_object_mut()
                .ok_or_else(|| invalid("response.completed response must be an object"))?
                .insert("output".to_owned(), Value::Array(items));
            recovered = Some(snapshot);
        }
        let response = recovered.as_ref().unwrap_or(response);
        let result = response::complete(
            response,
            &self.binding,
            &self.tools,
            request_id.or_else(|| self.response_id.clone()),
        )?;
        let output = required_array(response, "output")?;
        let mut events = Vec::new();
        let indices: Vec<usize> = if compacted {
            self.items.keys().copied().collect()
        } else {
            (0..output.len()).collect()
        };
        for (index, value) in indices.into_iter().zip(output) {
            if !self.items.contains_key(&index) {
                events.extend(self.add_item(index, value)?);
            }
            events.extend(
                self.items
                    .get_mut(&index)
                    .expect("item inserted")
                    .finish(value)?,
            );
        }
        if let Some(usage) = result.usage() {
            events.push(ModelStreamEvent::Usage(usage.clone()));
        }
        self.completed = true;
        events.push(ModelStreamEvent::Completed(result));
        Ok(events)
    }
}

fn index(value: &Value, field: &str) -> ModelResult<usize> {
    value
        .get(field)
        .and_then(Value::as_u64)
        .and_then(|value| usize::try_from(value).ok())
        .filter(|index| *index < 100_000)
        .ok_or_else(|| {
            invalid(&format!(
                "Responses field {field} must be a bounded nonnegative index"
            ))
        })
}
