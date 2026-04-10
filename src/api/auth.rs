use axum::extract::State;
use axum::http::{Request, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use subtle::ConstantTimeEq;

use crate::AppState;

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    sub: String,
    exp: usize,
}

#[derive(Deserialize)]
pub struct LoginRequest {
    pub password: String,
}

#[derive(Deserialize)]
pub struct PasswordChangeRequest {
    pub password: String,
}

fn generate_token(secret: &str) -> Result<String, jsonwebtoken::errors::Error> {
    let exp = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::days(30))
        .expect("valid timestamp")
        .timestamp() as usize;

    let claims = Claims {
        sub: "admin".to_string(),
        exp,
    };

    encode(
        &Header::default(),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
}

fn verify_token(token: &str, secret: &str) -> bool {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::default(),
    )
    .is_ok()
}

/// POST /api/login
pub async fn login(
    State(state): State<AppState>,
    Json(req): Json<LoginRequest>,
) -> impl IntoResponse {
    let expected = state.config.login_password.as_bytes();
    let provided = req.password.as_bytes();

    // Constant-time comparison
    let matches = if expected.len() == provided.len() {
        expected.ct_eq(provided).into()
    } else {
        false
    };

    if !matches {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({ "success": false, "message": "密码错误" })),
        );
    }

    match generate_token(&state.config.jwt_secret) {
        Ok(token) => (
            StatusCode::OK,
            Json(
                serde_json::json!({ "success": true, "message": "登录成功", "data": { "token": token } }),
            ),
        ),
        Err(_) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": "生成 token 失败" })),
        ),
    }
}

/// POST /api/password — Update login password (writes to .env file)
pub async fn update_password(
    State(_state): State<AppState>,
    Json(req): Json<PasswordChangeRequest>,
) -> impl IntoResponse {
    let password = req.password.trim();
    if password.len() < 4 {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "success": false, "message": "密码至少4个字符" })),
        );
    }

    match crate::api::admin::update_env_value("LOGIN_PASSWORD", password) {
        Ok(_) => (
            StatusCode::OK,
            Json(serde_json::json!({ "success": true, "message": "密码已更新，重启后生效" })),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "success": false, "message": format!("更新失败: {}", e) })),
        ),
    }
}

/// GET /api/auth/check — Public endpoint to check if auth is required
pub async fn auth_check(State(state): State<AppState>) -> impl IntoResponse {
    let required = !state.config.login_password.is_empty();
    Json(serde_json::json!({ "auth_required": required }))
}

/// Auth middleware — checks Authorization: Bearer <token> header
/// If no password is configured, all requests are allowed without auth.
pub async fn auth_middleware(
    State(state): State<AppState>,
    req: Request<axum::body::Body>,
    next: Next,
) -> Result<Response, StatusCode> {
    // No password set — skip auth entirely
    if state.config.login_password.is_empty() {
        return Ok(next.run(req).await);
    }

    let secret = &state.config.jwt_secret;

    // Check Authorization header
    if let Some(auth) = req
        .headers()
        .get("Authorization")
        .and_then(|h| h.to_str().ok())
    {
        if let Some(token) = auth.strip_prefix("Bearer ") {
            if verify_token(token, secret) {
                return Ok(next.run(req).await);
            }
        }
    }

    // Check query param ?token= (for WebSocket or simple access)
    if let Some(query) = req.uri().query() {
        for part in query.split('&') {
            if let Some(value) = part.strip_prefix("token=") {
                let token = value.trim();
                if !token.is_empty() && verify_token(token, secret) {
                    return Ok(next.run(req).await);
                }
            }
        }
    }

    Err(StatusCode::UNAUTHORIZED)
}
