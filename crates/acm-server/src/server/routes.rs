use axum::{
    Router,
    routing::{get, post},
};
use std::sync::Arc;
use tower_http::cors::{Any, CorsLayer};
use tower_http::trace::TraceLayer;

use super::state::AppState;

pub(super) fn build_router(state: Arc<AppState>) -> Router {
    Router::new()
        // Health check
        .route("/health", get(super::health_check))
        // Bootstrap script routes (content-negotiation + UA fallback)
        .route("/install/{provider}", get(super::script_install))
        .route("/{provider}/install.sh", get(super::script_install_sh))
        .route("/{provider}/install.ps1", get(super::script_install_ps1))
        .route("/update/{provider}", get(super::script_update))
        .route("/{provider}/update.sh", get(super::script_update_sh))
        .route("/{provider}/update.ps1", get(super::script_update_ps1))
        .route("/uninstall/{provider}", get(super::script_uninstall))
        .route("/{provider}/uninstall.sh", get(super::script_uninstall_sh))
        .route(
            "/{provider}/uninstall.ps1",
            get(super::script_uninstall_ps1),
        )
        .route("/status", get(super::script_status))
        .route("/status.sh", get(super::script_status_sh))
        .route("/status.ps1", get(super::script_status_ps1))
        .route("/doctor", get(super::script_doctor))
        .route("/doctor.sh", get(super::script_doctor_sh))
        .route("/doctor.ps1", get(super::script_doctor_ps1))
        // Generic artifact file route
        .route(
            "/{provider}/{version}/files/{*filepath}",
            get(super::provider_generic_file),
        )
        // Generic tag route
        .route("/{provider}/{tag}", get(super::provider_tag))
        // Generic API routes
        .route("/api/{provider}/info", get(super::api_provider_info))
        .route(
            "/api/{provider}/versions",
            get(super::api_provider_versions),
        )
        .route(
            "/api/{provider}/checksums",
            get(super::api_provider_checksums),
        )
        .route("/api/{provider}/refresh", post(super::api_provider_refresh))
        // Middleware
        .layer(TraceLayer::new_for_http())
        .layer(
            CorsLayer::new()
                .allow_origin(Any)
                .allow_methods([axum::http::Method::GET, axum::http::Method::POST])
                .allow_headers([axum::http::header::AUTHORIZATION, axum::http::header::RANGE]),
        )
        .with_state(state)
}
