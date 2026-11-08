use axum::extract::{Extension, Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use axum::{middleware, Json};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Map, Value};

use crate::admin::session::AdminSession;
use crate::api::{is_json_content_type, JsonBody};
use crate::models::time;
use crate::models::{AppError, Attachment, Category, MessageInput, Sender};
use crate::state::AppState;
use crate::util::constant_time_eq;

const LOGIN_HTML: &str = include_str!("../../templates/login.html");
const ADMIN_HTML: &str = include_str!("../../templates/admin.html");
const ADMIN_CSS: &str = include_str!("../../static/css/admin.css");
const ADMIN_JS: &str = include_str!("../../static/js/admin.js");

pub fn admin_router(state: AppState) -> Router<AppState> {
    let protected = Router::new()
        .route("/admin", get(admin_page))
        .route("/admin/logout", post(logout))
        .route("/admin/api/validate-player", post(validate_player))
        .route("/admin/api/messages", post(send_message))
        .route("/admin/api/broadcasts/:broadcast_id", get(broadcast_status))
        .route_layer(middleware::from_fn_with_state(state, require_admin));
    Router::new()
        .route("/admin/login", get(login_page))
        .route("/admin/auth/google", post(google_auth))
        .route("/static/css/admin.css", get(css))
        .route("/static/js/admin.js", get(javascript))
        .merge(protected)
}

async fn require_admin(
    State(state): State<AppState>,
    mut request: axum::extract::Request,
    next: Next,
) -> Response {
    let Some(session) = state.sessions.read(request.headers()) else {
        if request.uri().path() == "/admin" {
            return Redirect::to("/admin/login?error=session").into_response();
        }
        return AppError::session_required().into_response();
    };
    if request.method() != axum::http::Method::GET && request.method() != axum::http::Method::HEAD {
        let presented = request
            .headers()
            .get("x-csrf-token")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !constant_time_eq(presented.as_bytes(), session.csrf.as_bytes()) {
            return AppError::csrf().into_response();
        }
    }
    request.extensions_mut().insert(session);
    next.run(request).await
}

async fn login_page(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(query): Query<LoginQuery>,
) -> Response {
    if state.sessions.read(&headers).is_some() {
        return Redirect::to("/admin").into_response();
    }
    let error = match query.error.as_deref() {
        Some("access_denied") => "This Google account is not authorized to administer the mailbox.",
        Some("google") => "Google sign-in could not be verified. Try again.",
        Some("session") => "Your session expired. Sign in again.",
        _ => "",
    };
    let error_block = if error.is_empty() {
        String::new()
    } else {
        format!(
            "<p class=\"banner error\" role=\"alert\">{}</p>",
            escape_html(error)
        )
    };
    let html = LOGIN_HTML
        .replace("%%CLIENT_ID%%", &escape_html(&state.config.google_oauth_client_id))
        .replace("%%ERROR_BLOCK%%", &error_block);
    html_response(html, login_csp())
}

async fn google_auth(
    State(state): State<AppState>,
    headers: HeaderMap,
    body: JsonBody<GoogleCredential>,
) -> Result<Response, AppError> {
    if !is_json_content_type(headers.get(header::CONTENT_TYPE)) {
        return Err(AppError::unsupported_media());
    }
    let identity = state.google.verify(&body.value.credential).await?;
    state.google.authorize(&identity)?;
    let Some((cookie, _)) = state.sessions.issue(
        &identity.sub,
        identity.email,
        identity.name,
    ) else {
        return Err(AppError::internal());
    };
    let mut response = Json(serde_json::json!({ "ok": true })).into_response();
    if let Ok(value) = HeaderValue::from_str(&cookie) {
        response.headers_mut().insert(header::SET_COOKIE, value);
    }
    Ok(response)
}

async fn admin_page(Extension(session): Extension<AdminSession>) -> Response {
    let html = ADMIN_HTML
        .replace("%%CSRF%%", &escape_html(&session.csrf))
        .replace("%%ACCOUNT%%", &escape_html(&session.display_name()));
    html_response(html, admin_csp())
}

async fn logout(State(state): State<AppState>) -> Response {
    let mut response = Json(serde_json::json!({ "ok": true })).into_response();
    response
        .headers_mut()
        .insert(header::SET_COOKIE, state.sessions.clear_cookie());
    response
}

async fn validate_player(
    State(state): State<AppState>,
    body: JsonBody<ValidatePlayer>,
) -> Result<Json<Value>, AppError> {
    Ok(Json(state.broadcast.validate_player(&body.value.player_id).await?))
}

async fn send_message(
    State(state): State<AppState>,
    body: JsonBody<AdminSendRequest>,
) -> Result<Response, AppError> {
    let request = body.value;
    let input = MessageInput {
        sender: request.sender,
        category: request.category,
        subject: request.subject,
        body: request.body,
        expires_at: request.expires_at,
        attachments: request.attachments,
        metadata: request.metadata,
    };
    match request.mode.as_str() {
        "player" => {
            let player_id = request.player_id.unwrap_or_default();
            if player_id.is_empty() {
                return Err(AppError::validation("Enter a player ID before sending."));
            }
            let raw = serde_json::to_vec(&input).map_err(|_| AppError::internal())?;
            let outcome = state.mailbox.send(&player_id, &input, &raw, None).await?;
            Ok((
                StatusCode::CREATED,
                Json(serde_json::json!({
                    "delivery": "player",
                    "message": outcome.message
                })),
            )
                .into_response())
        }
        "broadcast" => {
            if !request.confirm_broadcast {
                return Err(AppError::validation(
                    "Confirm that this message should be sent to every enabled player.",
                ));
            }
            let raw = serde_json::to_vec(&input).map_err(|_| AppError::internal())?;
            let acceptance = state.broadcast.accept(&input, &raw, None).await?;
            let status = if acceptance.created {
                StatusCode::ACCEPTED
            } else {
                StatusCode::OK
            };
            Ok((
                status,
                Json(serde_json::json!({
                    "delivery": "broadcast",
                    "broadcastId": acceptance.broadcast_id,
                    "status": "pending",
                    "created": acceptance.created
                })),
            )
                .into_response())
        }
        _ => Err(AppError::validation(
            "Choose a specific player or all players.",
        )),
    }
}

async fn broadcast_status(
    State(state): State<AppState>,
    Path(broadcast_id): Path<String>,
) -> Result<Json<crate::models::AdminBroadcastStatus>, AppError> {
    let broadcast_id = uuid::Uuid::parse_str(&broadcast_id)
        .map_err(|_| AppError::validation("broadcastId must be a UUID."))?;
    Ok(Json(state.broadcast.admin_status(broadcast_id).await?))
}

async fn css() -> Response {
    (
        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
        ADMIN_CSS,
    )
        .into_response()
}

async fn javascript() -> Response {
    (
        [(
            header::CONTENT_TYPE,
            "text/javascript; charset=utf-8",
        )],
        ADMIN_JS,
    )
        .into_response()
}

fn html_response(html: String, csp: &'static str) -> Response {
    let mut response = (
        [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
        html,
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(csp),
    );
    response.headers_mut().insert(
        header::X_FRAME_OPTIONS,
        HeaderValue::from_static("DENY"),
    );
    response
}

fn login_csp() -> &'static str {
    "default-src 'self'; script-src 'self' https://accounts.google.com; frame-src https://accounts.google.com; style-src 'self' 'unsafe-inline'; img-src 'self' data: https://*.googleusercontent.com https://*.gstatic.com; connect-src 'self' https://accounts.google.com; base-uri 'self'; form-action 'self'; frame-ancestors 'none'"
}

fn admin_csp() -> &'static str {
    "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; base-uri 'self'; form-action 'self'; frame-ancestors 'none'"
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[derive(Deserialize)]
struct LoginQuery {
    error: Option<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct GoogleCredential {
    credential: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ValidatePlayer {
    player_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct AdminSendRequest {
    mode: String,
    #[serde(default)]
    player_id: Option<String>,
    #[serde(default)]
    confirm_broadcast: bool,
    sender: Sender,
    category: Category,
    subject: String,
    body: String,
    #[serde(default, with = "time::optional")]
    expires_at: Option<DateTime<Utc>>,
    #[serde(default)]
    attachments: Vec<Attachment>,
    #[serde(default)]
    metadata: Map<String, Value>,
}

