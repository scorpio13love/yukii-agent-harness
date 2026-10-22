//! HTTP Gateway 边界：回环 Host/Origin 限制、请求关联 ID 与脱敏访问日志。
use axum::{
    Json,
    extract::Request,
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use std::time::Instant;

/// 只接受已知开发入口，防止 DNS rebinding 与第三方网页调用本机 API。
/// 这不替代用户认证；公网部署必须先接入认证和租户授权层。
pub async fn request_boundary(request: Request, next: Next) -> Response {
    let request_id = uuid::Uuid::new_v4().to_string();
    let started = Instant::now();
    let method = request.method().clone();
    let host = request.headers().get("host").and_then(|v| v.to_str().ok());
    let allowed_host = matches!(
        host,
        Some("127.0.0.1:8765" | "localhost:8765" | "127.0.0.1:5173" | "localhost:5173")
    );
    let origin = request.headers().get("origin");
    let allowed_origin = origin.is_none()
        || origin.and_then(|v| v.to_str().ok()).is_some_and(|value| {
            matches!(
                value,
                "http://localhost:5173"
                    | "http://127.0.0.1:5173"
                    | "http://localhost:8765"
                    | "http://127.0.0.1:8765"
            )
        });
    let cross_site = request
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|value| value == "cross-site");
    let mut response = if !allowed_host || !allowed_origin || cross_site {
        (
            StatusCode::FORBIDDEN,
            Json(serde_json::json!({"code":"forbidden_origin","message":"不允许的请求来源"})),
        )
            .into_response()
    } else {
        next.run(request).await
    };
    if let Ok(value) = HeaderValue::from_str(&request_id) {
        response.headers_mut().insert("x-request-id", value);
    }
    response
        .headers_mut()
        .insert("cache-control", HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    // 不记录提示词、响应正文、查询参数、认证头或供应商密钥。
    tracing::info!(%request_id,%method,status=response.status().as_u16(),elapsed_ms=started.elapsed().as_millis(),"HTTP 请求完成");
    response
}
