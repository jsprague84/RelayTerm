use axum::{Json, Router, routing::get};
use serde::Serialize;

use crate::AppState;

#[derive(Serialize)]
struct Health {
    status: &'static str,
}

async fn healthz() -> Json<Health> {
    Json(Health { status: "ok" })
}

pub(crate) fn router() -> Router<AppState> {
    Router::new().route("/healthz", get(healthz))
}
