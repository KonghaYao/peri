use std::collections::{BTreeMap, BTreeSet};

use serde_json::{json, Value};
use url::Url;

use crate::{
    ModelError, ModelRequest, ModelResult, ModelRuntimeConfig, PreparedModelRequest,
    ProtocolErrorKind, ProviderProtocol,
};

use super::{
    replay::ReplayBinding, request_content::messages_to_input, ResponsesConfig, ResponsesProfile,
    TOOL_NAMESPACE,
};

pub(super) struct BuiltResponsesRequest {
    pub endpoint: Url,
    pub body: Value,
    pub binding: ReplayBinding,
    pub tool_names: BTreeSet<String>,
}

impl BuiltResponsesRequest {
    pub(super) fn observe(
        &self,
        runtime: &ModelRuntimeConfig,
    ) -> ModelResult<PreparedModelRequest> {
        PreparedModelRequest::observe_with_runtime(
            ProviderProtocol::OpenAiResponses,
            self.binding.model.clone(),
            self.endpoint.clone(),
            self.body.clone(),
            BTreeMap::new(),
            runtime,
        )
    }
}

pub(super) fn build_request(
    config: &ResponsesConfig,
    request: &ModelRequest,
) -> ModelResult<BuiltResponsesRequest> {
    let endpoint = responses_endpoint(&config.endpoint, config.profile)?;
    if let Some(auth) = &config.auth {
        auth.validate_endpoint(&endpoint)?;
    }
    if config.model.trim().is_empty() || config.credential_ref.is_empty() {
        return Err(invalid(
            "Responses model and credential identity must not be empty",
        ));
    }
    let binding = ReplayBinding {
        endpoint: endpoint.as_str().to_owned(),
        model: config.model.clone(),
        credential_ref: config.credential_ref.clone(),
        profile: config.profile,
    };
    let mut body = json!({
        "model": config.model,
        "input": messages_to_input(&request.messages, &binding)?,
        "stream": true,
        "store": false,
        "include": ["reasoning.encrypted_content"],
    });
    let mut tool_names = BTreeSet::new();
    let mut functions = Vec::new();
    for tool in &request.tools {
        if !valid_tool_name(&tool.name) || !tool_names.insert(tool.name.clone()) {
            return Err(invalid(
                "Responses requires unique function names of 1-64 ASCII letters, digits, underscores or hyphens",
            ));
        }
        let mut function = json!({
            "type": "function", "name": tool.name,
            "parameters": tool.input_schema, "strict": false,
        });
        if let Some(description) = &tool.description {
            function["description"] = json!(description);
        }
        functions.push(function);
    }
    if !functions.is_empty() {
        body["tools"] = match config.profile {
            ResponsesProfile::Platform => Value::Array(functions),
            ResponsesProfile::ChatGptPlan => json!([{
                "type": "namespace", "name": TOOL_NAMESPACE,
                "description": "Tools provided by the Peri host", "tools": functions,
            }]),
        };
        body["tool_choice"] = json!("auto");
    }
    if let Some(effort) = &config.reasoning_effort {
        if !matches!(
            effort.as_str(),
            "none" | "minimal" | "low" | "medium" | "high" | "xhigh" | "max"
        ) {
            return Err(invalid("Unsupported Responses reasoning effort"));
        }
        body["reasoning"] = json!({"effort": effort, "summary": "auto"});
    }
    if config.profile == ResponsesProfile::Platform {
        if let Some(max_tokens) = request.max_tokens.or(config.max_tokens) {
            if max_tokens == 0 {
                return Err(invalid("max_output_tokens must be greater than zero"));
            }
            body["max_output_tokens"] = json!(max_tokens);
        }
        if let Some(temperature) = request.temperature {
            if !temperature.is_finite() {
                return Err(invalid("temperature must be finite"));
            }
            body["temperature"] = json!(temperature);
        }
    }
    Ok(BuiltResponsesRequest {
        endpoint,
        body,
        binding,
        tool_names,
    })
}

fn responses_endpoint(endpoint: &Url, profile: ResponsesProfile) -> ModelResult<Url> {
    if !matches!(endpoint.scheme(), "http" | "https")
        || endpoint.host_str().is_none()
        || !endpoint.username().is_empty()
        || endpoint.password().is_some()
        || endpoint.fragment().is_some()
        || endpoint.query().is_some()
    {
        return Err(ModelError::protocol_with_summary(
            ProtocolErrorKind::InvalidEndpoint,
            "Responses requires an HTTP(S) endpoint without credentials, query or fragment",
        ));
    }
    let mut endpoint = endpoint.clone();
    if !endpoint
        .path()
        .trim_end_matches('/')
        .ends_with("/responses")
    {
        let mut segments = endpoint
            .path_segments_mut()
            .map_err(|_| invalid("Endpoint has no hierarchical path"))?;
        segments.pop_if_empty().push("responses");
    } else {
        let path = endpoint.path().trim_end_matches('/').to_owned();
        endpoint.set_path(&path);
    }
    if profile == ResponsesProfile::ChatGptPlan
        && endpoint.as_str() != "https://api.openai.com/v1/responses"
    {
        return Err(ModelError::protocol_with_summary(
            ProtocolErrorKind::InvalidEndpoint,
            "ChatGPT plan credentials require https://api.openai.com/v1/responses",
        ));
    }
    Ok(endpoint)
}

pub(super) fn valid_tool_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn invalid(message: &str) -> ModelError {
    ModelError::protocol_with_summary(ProtocolErrorKind::ProviderTerminal, message)
}
