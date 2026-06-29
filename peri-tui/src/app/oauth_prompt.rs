use super::events::OAuthCallbackResult;
use crate::app::FieldTextarea;

// P4b: inline parse_code_from_url replaces peri_middlewares::mcp::parse_code_from_url
fn parse_code_from_url(raw: &str) -> Result<(String, String), String> {
    let query_start = raw.find('?').unwrap_or(raw.len());
    let query = if query_start < raw.len() {
        &raw[query_start + 1..]
    } else {
        ""
    };
    let mut code: Option<String> = None;
    let mut state: Option<String> = None;
    for pair in query.split('&') {
        let mut parts = pair.splitn(2, '=');
        let key = parts.next().unwrap_or("");
        let value = parts.next().unwrap_or("");
        let decoded = value.replace('+', " ").replace("%20", " ");
        match key {
            "code" => code = Some(decoded),
            "state" => state = Some(decoded),
            _ => {}
        }
    }
    let code = code.ok_or_else(|| "URL 缺少 code 参数".to_string())?;
    let state = state.ok_or_else(|| "URL 缺少 state 参数".to_string())?;
    Ok((code, state))
}

pub struct OAuthPrompt {
    pub server_name: String,
    pub authorization_url: String,
    pub field: FieldTextarea,
    pub callback_tx: Option<tokio::sync::oneshot::Sender<OAuthCallbackResult>>,
    pub error_message: Option<String>,
}

impl OAuthPrompt {
    pub fn new(
        server_name: String,
        authorization_url: String,
        callback_tx: tokio::sync::oneshot::Sender<OAuthCallbackResult>,
    ) -> Self {
        Self {
            server_name,
            authorization_url,
            field: FieldTextarea::single_line(),
            callback_tx: Some(callback_tx),
            error_message: None,
        }
    }

    pub fn submit(&mut self) -> bool {
        match parse_code_from_url(&self.field.value()) {
            Ok((code, state)) => {
                if let Some(tx) = self.callback_tx.take() {
                    let _ = tx.send(OAuthCallbackResult { code, state });
                }
                true
            }
            Err(e) => {
                self.error_message = Some(format!("Unable to parse callback URL: {}", e));
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    include!("oauth_prompt_test.rs");
}
