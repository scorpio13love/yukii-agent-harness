//! 办公扩展应用接口。端点只协调服务，不在 HTTP 层实现分词、安装或容器进程。
use super::AppState;
use axum::{
    Json, Router,
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use std::sync::Arc;
use yukii_agent_extensions::{ExtensionError, ExtensionManager};
use yukii_agent_ports::repository::RepositoryError;
use yukii_agent_ports::sandbox::Sandbox;
use yukii_agent_rag::RagService;
use yukii_contracts::{
    extensions::{InstallPlugin, SetPluginEnabled},
    knowledge::{ImportDocument, KnowledgeQuery},
};

pub struct OfficeServices {
    pub rag: Arc<RagService>,
    pub extensions: Arc<ExtensionManager>,
    pub sandbox: Arc<dyn Sandbox>,
}
pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/api/knowledge/documents",
            get(documents).post(import_document),
        )
        .route(
            "/api/knowledge/documents/{id}",
            axum::routing::delete(delete_document),
        )
        .route("/api/knowledge/search", post(search))
        .route("/api/knowledge/index", get(index_status).post(retry_index))
        .route("/api/plugins", get(plugins))
        .route("/api/plugins/install", post(install))
        .route("/api/plugins/{id}/enabled", post(enable))
        .route("/api/marketplace", get(marketplace))
        .route("/api/marketplace/{id}/{version}", get(preview))
        .route("/api/sandbox/status", get(sandbox_status))
}
enum OfficeError {
    Repository(RepositoryError),
    Extension(ExtensionError),
    Market(String),
    Unavailable,
}
impl From<RepositoryError> for OfficeError {
    fn from(error: RepositoryError) -> Self {
        Self::Repository(error)
    }
}
impl From<ExtensionError> for OfficeError {
    fn from(error: ExtensionError) -> Self {
        Self::Extension(error)
    }
}
impl IntoResponse for OfficeError {
    fn into_response(self) -> Response {
        let (status, code, message) = match self {
            Self::Repository(error) | Self::Extension(ExtensionError::Repository(error)) => {
                return super::HttpError(error).into_response();
            }
            Self::Unavailable => (
                StatusCode::SERVICE_UNAVAILABLE,
                "office_unavailable",
                "办公扩展服务未配置".to_owned(),
            ),
            Self::Extension(ExtensionError::SandboxUnavailable) => (
                StatusCode::SERVICE_UNAVAILABLE,
                "sandbox_unavailable",
                "Docker 沙箱不可用，不能启用容器工具".to_owned(),
            ),
            Self::Extension(ExtensionError::InvalidRequest(message)) => (
                StatusCode::BAD_REQUEST,
                "invalid_plugin_request",
                message.to_owned(),
            ),
            Self::Market(message) | Self::Extension(ExtensionError::Marketplace(message)) => {
                (StatusCode::BAD_GATEWAY, "marketplace_error", message)
            }
        };
        (
            status,
            Json(serde_json::json!({"code":code,"message":message})),
        )
            .into_response()
    }
}
fn service(state: AppState) -> Result<Arc<OfficeServices>, OfficeError> {
    state.office.ok_or(OfficeError::Unavailable)
}
async fn documents(State(state): State<AppState>) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(service(state)?.rag.documents().await?))
}
async fn import_document(
    State(state): State<AppState>,
    Json(input): Json<ImportDocument>,
) -> Result<impl IntoResponse, OfficeError> {
    Ok((
        StatusCode::CREATED,
        Json(service(state)?.rag.import(input).await?),
    ))
}
async fn delete_document(
    State(state): State<AppState>,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, OfficeError> {
    service(state)?.rag.delete(id).await?;
    Ok(StatusCode::NO_CONTENT)
}
async fn search(
    State(state): State<AppState>,
    Json(input): Json<KnowledgeQuery>,
) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(service(state)?.rag.search_report(&input.query).await?))
}
async fn index_status(State(state): State<AppState>) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(service(state)?.rag.index_status().await?))
}
async fn retry_index(State(state): State<AppState>) -> Result<impl IntoResponse, OfficeError> {
    Ok((
        StatusCode::ACCEPTED,
        Json(service(state)?.rag.retry_index().await?),
    ))
}
async fn plugins(State(state): State<AppState>) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(
        service(state)?
            .extensions
            .installed()
            .await
            .map_err(OfficeError::from)?,
    ))
}
async fn install(
    State(state): State<AppState>,
    Json(input): Json<InstallPlugin>,
) -> Result<impl IntoResponse, OfficeError> {
    Ok((
        StatusCode::CREATED,
        Json(
            service(state)?
                .extensions
                .install(input)
                .await
                .map_err(OfficeError::from)?,
        ),
    ))
}
async fn enable(
    State(state): State<AppState>,
    Path(id): Path<String>,
    Json(input): Json<SetPluginEnabled>,
) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(
        service(state)?
            .extensions
            .set_enabled(id, input)
            .await
            .map_err(OfficeError::from)?,
    ))
}
async fn marketplace(State(state): State<AppState>) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(
        service(state)?
            .extensions
            .marketplace()
            .catalog()
            .await
            .map_err(OfficeError::Market)?,
    ))
}
async fn preview(
    State(state): State<AppState>,
    Path((id, version)): Path<(String, String)>,
) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(
        service(state)?
            .extensions
            .marketplace()
            .preview(&id, &version)
            .await
            .map_err(OfficeError::Market)?,
    ))
}
async fn sandbox_status(State(state): State<AppState>) -> Result<impl IntoResponse, OfficeError> {
    Ok(Json(service(state)?.sandbox.status().await))
}
