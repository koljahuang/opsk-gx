use axum::{
    Json,
    extract::{Path, Query, State},
    response::sse::{Event, Sse},
};
use std::convert::Infallible;
use std::time::Duration;
use tokio_stream::{Stream, StreamExt, wrappers::BroadcastStream};
use uuid::Uuid;

use crate::AppState;
use crate::error::AppResult;
use crate::middleware::auth::AuthUser;
use crate::models::notification::{Notification, NotificationListQuery};
use crate::services;

/// GET /api/notifications
pub async fn list(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Query(query): Query<NotificationListQuery>,
) -> AppResult<Json<Vec<Notification>>> {
    let limit = query.limit.unwrap_or(50).min(200);
    let offset = query.offset.unwrap_or(0);
    let notifications = services::notification::list(&state.pool, auth_user.user_id, limit, offset).await?;
    Ok(Json(notifications))
}

/// GET /api/notifications/unread-count
pub async fn unread_count(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let count = services::notification::count_unread(&state.pool, auth_user.user_id).await?;
    Ok(Json(serde_json::json!({ "count": count })))
}

/// POST /api/notifications/:id/read
pub async fn mark_read(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
    Path(id): Path<Uuid>,
) -> AppResult<Json<serde_json::Value>> {
    let updated = services::notification::mark_read(&state.pool, auth_user.user_id, id).await?;
    Ok(Json(serde_json::json!({ "success": updated })))
}

/// POST /api/notifications/read-all
pub async fn mark_all_read(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> AppResult<Json<serde_json::Value>> {
    let count = services::notification::mark_all_read(&state.pool, auth_user.user_id).await?;
    Ok(Json(serde_json::json!({ "updated": count })))
}

/// GET /api/notifications/stream — SSE real-time notification stream.
/// Filters broadcast channel to only deliver notifications for the authenticated user.
pub async fn stream(
    auth_user: axum::Extension<AuthUser>,
    State(state): State<AppState>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let user_id = auth_user.user_id;
    let rx = state.notification_tx.subscribe();

    let stream = BroadcastStream::new(rx).filter_map(move |result| {
        match result {
            Ok(notification) if notification.user_id == user_id => {
                let event = Event::default()
                    .json_data(&notification)
                    .unwrap_or_else(|_| Event::default().data("{}"));
                Some(Ok(event))
            }
            _ => None, // skip errors (lagged) and notifications for other users
        }
    });

    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("ping"),
    )
}
