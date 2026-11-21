use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use super::{AppState, CONTRACT_HEADER, CONTRACT_VERSION, EXPECT_HEADER, OPENAI_BETA_HEADER};
use crate::domain::SwarmError;

/// Contract, deployment-key and Beta-header gate shared by every route.
pub async fn guard(State(state): State<AppState>, request: Request, next: Next) -> Response {
    if let Err(error) = check_contract(&request) {
        return finish(error.into_response());
    }
    if let Err(error) = check_auth(&state, &request) {
        let mut response = error.into_response();
        if response.status() == StatusCode::UNAUTHORIZED {
            response
                .headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
        }
        return finish(response);
    }
    if let Err(error) = check_beta(&request) {
        return finish(error.into_response());
    }
    finish(next.run(request).await)
}

fn finish(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(CONTRACT_HEADER, HeaderValue::from_static(CONTRACT_VERSION));
    response
}

fn check_contract(request: &Request) -> Result<(), SwarmError> {
    let Some(value) = request.headers().get(EXPECT_HEADER) else {
        return Ok(());
    };
    let text = value
        .to_str()
        .map_err(|_| SwarmError::unsupported_contract("<non-ascii>"))?;
    if text.trim() == CONTRACT_VERSION {
        Ok(())
    } else {
        Err(SwarmError::unsupported_contract(text))
    }
}

fn check_auth(state: &AppState, request: &Request) -> Result<(), SwarmError> {
    let Some(value) = request.headers().get(AUTHORIZATION) else {
        return Err(SwarmError::unauthorized());
    };
    let text = value.to_str().map_err(|_| SwarmError::unauthorized())?;
    let Some((scheme, token)) = text.split_once(' ') else {
        return Err(SwarmError::unauthorized());
    };
    if !scheme.eq_ignore_ascii_case("bearer") {
        return Err(SwarmError::unauthorized());
    }
    if state.config().api_key.verify(token) {
        Ok(())
    } else {
        Err(SwarmError::unauthorized())
    }
}

fn check_beta(request: &Request) -> Result<(), SwarmError> {
    let Some(value) = request.headers().get(OPENAI_BETA_HEADER) else {
        return Ok(());
    };
    let text = value
        .to_str()
        .map_err(|_| SwarmError::unsupported_beta("<non-ascii>"))?;
    if text.trim() == "agents=v1" {
        Ok(())
    } else {
        Err(SwarmError::unsupported_beta(text))
    }
}
