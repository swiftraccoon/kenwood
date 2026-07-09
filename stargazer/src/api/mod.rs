//! HTTP API server for operational visibility.
//!
//! Provides endpoints for monitoring stargazer's internal state, querying
//! captured data, and manual Tier 3 session management. This is NOT the
//! primary data consumer — the Rdio API server handles transcription
//! downstream. The routes here exist so operators (and kubernetes health
//! probes) can answer "is capture working?" and "what did we miss?" at
//! a glance.
//!
//! # Endpoints
//!
//! | Route | Method | Purpose |
//! |-------|--------|---------|
//! | `/health` | GET | Kubernetes liveness/readiness probe |
//! | `/metrics` | GET | Tier statistics: reflectors, streams, upload queue |
//! | `/api/reflectors` | GET | List reflectors with status, most recent first |
//! | `/api/reflectors/{callsign}/activity` | GET | Recent activity for one reflector |
//! | `/api/reflectors/{callsign}/nodes` | GET | Nodes currently linked to a reflector |
//! | `/api/activity` | GET | Recent activity across all reflectors |
//! | `/api/streams` | GET | Query captured streams with filters |
//! | `/api/upload-queue` | GET | Pending upload status |
//! | `/api/tier3/connect` | POST | Manually promote a reflector to Tier 3 (501 stub) |
//! | `/api/tier3/{callsign}/{module}` | DELETE | Disconnect a Tier 3 session (501 stub) |
//!
//! # Error handling
//!
//! Database errors are logged at `warn` level and surfaced to the caller
//! as `500 Internal Server Error` with no body. The raw `sqlx::Error` is
//! never leaked — it can contain connection strings, schema details, or
//! constraint names that would be useful to an attacker.
//!
//! # Operational ownership
//!
//! The server is spawned by `stargazer::run` as a top-level tokio task.
//! On shutdown it is aborted; no graceful drain is attempted because all
//! endpoints are idempotent reads.

mod routes;

use std::net::SocketAddr;

use axum::Router;
use axum::routing::{delete, get, post};
use tokio::net::TcpListener;

/// Starts the HTTP API server and listens for requests.
///
/// Binds to the given `listen` address and serves the operational
/// visibility endpoints documented at the module level. Runs until the
/// caller aborts the returned future (e.g. during SIGTERM handling in
/// `main`).
///
/// # Errors
///
/// Returns an error if:
/// - The TCP listener cannot bind (port in use, permission denied).
/// - `axum::serve` returns an I/O error (effectively never — see the
///   axum docs, the underlying future currently never completes).
pub(crate) async fn serve(
    listen: SocketAddr,
    pool: sqlx::PgPool,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let listener = TcpListener::bind(listen).await?;
    let bound = listener.local_addr()?;
    tracing::info!(listen = %bound, "HTTP API server listening");

    let router = build_router(pool);
    axum::serve(listener, router).await?;
    Ok(())
}

/// Builds the axum `Router` with all routes and shared state.
///
/// Extracted from [`serve`] so the route table can be exercised without
/// standing up a TCP listener — the unit tests drive it with
/// `tower::ServiceExt::oneshot`.
fn build_router(pool: sqlx::PgPool) -> Router {
    Router::new()
        .route("/health", get(routes::health))
        .route("/metrics", get(routes::metrics))
        .route("/api/reflectors", get(routes::list_reflectors))
        .route(
            "/api/reflectors/{callsign}/activity",
            get(routes::reflector_activity),
        )
        .route(
            "/api/reflectors/{callsign}/nodes",
            get(routes::reflector_nodes),
        )
        .route("/api/activity", get(routes::list_activity))
        .route("/api/streams", get(routes::list_streams))
        .route("/api/upload-queue", get(routes::upload_queue))
        .route("/api/tier3/connect", post(routes::tier3_connect))
        .route(
            "/api/tier3/{callsign}/{module}",
            delete(routes::tier3_disconnect),
        )
        .with_state(pool)
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use sqlx::postgres::PgPoolOptions;
    use tower::ServiceExt;

    use super::build_router;

    #[tokio::test]
    async fn router_routes_health_endpoint() -> Result<(), Box<dyn std::error::Error>> {
        // A lazy pool never opens a connection until a query runs, and the
        // `/health` handler issues none — so this drives the real route table
        // and handler end to end without a live Postgres.
        let pool = PgPoolOptions::new().connect_lazy("postgres://stargazer@localhost/stargazer")?;
        let router = build_router(pool);

        let response = router
            .oneshot(Request::builder().uri("/health").body(Body::empty())?)
            .await?;

        assert_eq!(response.status(), StatusCode::OK);
        Ok(())
    }
}
