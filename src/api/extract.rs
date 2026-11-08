use axum::body::Bytes;
use axum::extract::{FromRequest, Request};
use axum::http::header;
use axum::response::IntoResponse;
use serde::de::DeserializeOwned;

use crate::api::middleware::is_json_content_type;
use crate::models::AppError;
use crate::validation::MAX_BODY_BYTES;

pub struct JsonBody<T> {
    pub value: T,
    pub raw: Bytes,
}

#[axum::async_trait]
impl<S, T> FromRequest<S> for JsonBody<T>
where
    S: Send + Sync,
    T: DeserializeOwned,
{
    type Rejection = AppError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        if !is_json_content_type(request.headers().get(header::CONTENT_TYPE)) {
            return Err(AppError::unsupported_media());
        }
        let bytes = match Bytes::from_request(request, state).await {
            Ok(bytes) => bytes,
            Err(rejection) => {
                let response = rejection.into_response();
                if response.status() == axum::http::StatusCode::PAYLOAD_TOO_LARGE {
                    return Err(AppError::payload_too_large());
                }
                return Err(AppError::invalid_json("The request body could not be read."));
            }
        };
        if bytes.len() > MAX_BODY_BYTES {
            return Err(AppError::payload_too_large());
        }
        if bytes.is_empty() {
            return Err(AppError::invalid_json("The request body is empty."));
        }
        let value = match serde_json::from_slice::<T>(&bytes) {
            Ok(value) => value,
            Err(error) if error.is_syntax() || error.is_eof() => {
                return Err(AppError::invalid_json(format!(
                    "The request JSON is invalid: {}",
                    brief_error(&error)
                )));
            }
            Err(error) => {
                return Err(AppError::validation(format!(
                    "Validation failed: {}",
                    brief_error(&error)
                )));
            }
        };
        Ok(Self { value, raw: bytes })
    }
}

fn brief_error(error: &serde_json::Error) -> String {
    let text = error.to_string();
    let mut characters = text.chars();
    let short: String = characters.by_ref().take(300).collect();
    if characters.next().is_some() {
        format!("{short}...")
    } else {
        short
    }
}

pub fn require_empty_body(body: &Bytes) -> Result<(), AppError> {
    if body.is_empty() {
        Ok(())
    } else {
        Err(AppError::invalid_json(
            "This endpoint does not accept a request body.",
        ))
    }
}
