//! Reusable usage query JSON contract for API and worker routes.

use std::sync::Arc;

use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    Json,
};
use llm_access_core::{
    store::{UsageAnalyticsStore, UsageEventPage, UsageEventQuery, UsageEventSource},
    usage::UsageEvent,
};
use serde::{Deserialize, Serialize};

const DEFAULT_ADMIN_USAGE_LIMIT: usize = 20;
const MAX_ADMIN_USAGE_LIMIT: usize = 20;
const MAX_ADMIN_USAGE_OFFSET: usize = 200;

/// Query options for usage list endpoints.
#[derive(Debug, Clone, Default, Deserialize)]
pub(crate) struct ListUsageEventsRequest {
    #[serde(default)]
    key_id: Option<String>,
    #[serde(default)]
    start_ms: Option<i64>,
    #[serde(default)]
    end_ms: Option<i64>,
    #[serde(default)]
    source: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    #[serde(default)]
    offset: Option<usize>,
}

/// Paginated usage response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminUsageEventsResponse {
    total: usize,
    offset: usize,
    limit: usize,
    has_more: bool,
    current_rpm: u32,
    current_in_flight: u32,
    events: Vec<AdminUsageEventView>,
    generated_at: i64,
}

/// Summary usage event.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminUsageEventView {
    id: String,
    key_id: String,
    key_name: String,
    account_name: Option<String>,
    request_method: String,
    request_url: String,
    latency_ms: i32,
    routing_wait_ms: Option<i32>,
    upstream_headers_ms: Option<i32>,
    post_headers_body_ms: Option<i32>,
    request_body_bytes: Option<u64>,
    request_body_read_ms: Option<i32>,
    request_json_parse_ms: Option<i32>,
    pre_handler_ms: Option<i32>,
    first_sse_write_ms: Option<i32>,
    stream_finish_ms: Option<i32>,
    stream_completed_cleanly: Option<bool>,
    downstream_disconnect: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    final_event_type: Option<String>,
    bytes_streamed: Option<u64>,
    other_latency_ms: Option<i32>,
    quota_failover_count: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    routing_diagnostics_json: Option<String>,
    endpoint: String,
    model: Option<String>,
    status_code: i32,
    input_uncached_tokens: u64,
    input_cached_tokens: u64,
    output_tokens: u64,
    billable_tokens: u64,
    usage_missing: bool,
    credit_usage: Option<f64>,
    credit_usage_missing: bool,
    client_ip: String,
    ip_region: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_message_content: Option<String>,
    created_at: i64,
}

/// Usage detail response.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AdminUsageEventDetailView {
    #[serde(flatten)]
    event: AdminUsageEventView,
    request_headers_json: String,
    client_request_body_json: Option<String>,
    upstream_request_body_json: Option<String>,
    full_request_json: Option<String>,
}

/// Worker query route state.
#[derive(Clone)]
pub(crate) struct UsageQueryState {
    pub(crate) usage_analytics_store: Arc<dyn UsageAnalyticsStore>,
}

/// List all LLM usage events.
pub(crate) async fn list_llm_usage_events(
    State(state): State<UsageQueryState>,
    Query(request): Query<ListUsageEventsRequest>,
) -> Response {
    list_usage_events(state, request, None).await
}

/// List Kiro usage events.
pub(crate) async fn list_kiro_usage_events(
    State(state): State<UsageQueryState>,
    Query(request): Query<ListUsageEventsRequest>,
) -> Response {
    list_usage_events(state, request, Some("kiro")).await
}

/// Return one LLM usage event.
pub(crate) async fn get_llm_usage_event(
    State(state): State<UsageQueryState>,
    Path(event_id): Path<String>,
) -> Response {
    get_usage_event(state, event_id, None).await
}

/// Return one Kiro usage event.
pub(crate) async fn get_kiro_usage_event(
    State(state): State<UsageQueryState>,
    Path(event_id): Path<String>,
) -> Response {
    get_usage_event(state, event_id, Some("kiro")).await
}

async fn list_usage_events(
    state: UsageQueryState,
    request: ListUsageEventsRequest,
    provider_type: Option<&str>,
) -> Response {
    let query = match normalize_usage_query(request, provider_type) {
        Ok(query) => query,
        Err(message) => return (StatusCode::BAD_REQUEST, message).into_response(),
    };
    match state.usage_analytics_store.list_usage_events(query).await {
        Ok(page) => Json(response_from_page(page)).into_response(),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to list usage events: {err:#}"),
        )
            .into_response(),
    }
}

async fn get_usage_event(
    state: UsageQueryState,
    event_id: String,
    provider_type: Option<&str>,
) -> Response {
    match state.usage_analytics_store.get_usage_event(&event_id).await {
        Ok(Some(event))
            if provider_type.is_none()
                || provider_type == Some(event.provider_type.as_storage_str()) =>
        {
            Json(detail_from_event(&event)).into_response()
        },
        Ok(_) => axum::http::StatusCode::NOT_FOUND.into_response(),
        Err(err) => (
            axum::http::StatusCode::INTERNAL_SERVER_ERROR,
            format!("Failed to load usage event: {err:#}"),
        )
            .into_response(),
    }
}

fn response_from_page(page: UsageEventPage) -> AdminUsageEventsResponse {
    AdminUsageEventsResponse {
        total: page.total,
        offset: page.offset,
        limit: page.limit,
        has_more: page.has_more,
        current_rpm: 0,
        current_in_flight: 0,
        events: page.events.iter().map(AdminUsageEventView::from).collect(),
        generated_at: now_ms(),
    }
}

fn detail_from_event(event: &UsageEvent) -> AdminUsageEventDetailView {
    AdminUsageEventDetailView {
        event: AdminUsageEventView::from(event),
        request_headers_json: event.request_headers_json.clone(),
        client_request_body_json: event.client_request_body_json.clone(),
        upstream_request_body_json: event.upstream_request_body_json.clone(),
        full_request_json: event.full_request_json.clone(),
    }
}

fn normalize_usage_query(
    request: ListUsageEventsRequest,
    provider_type: Option<&str>,
) -> Result<UsageEventQuery, &'static str> {
    let (start_ms, end_ms) = normalize_usage_time_range(request.start_ms, request.end_ms);
    let source = match request
        .source
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        Some(value) => UsageEventSource::from_query_value(value)
            .ok_or("source must be one of hot, archive, or all")?,
        None => UsageEventSource::Hot,
    };
    Ok(UsageEventQuery {
        key_id: request
            .key_id
            .and_then(|value| normalize_optional_string(&value)),
        provider_type: provider_type.map(str::to_string),
        source,
        start_ms,
        end_ms,
        limit: request
            .limit
            .unwrap_or(DEFAULT_ADMIN_USAGE_LIMIT)
            .clamp(1, MAX_ADMIN_USAGE_LIMIT),
        offset: request.offset.unwrap_or(0).min(MAX_ADMIN_USAGE_OFFSET),
    })
}

impl From<&UsageEvent> for AdminUsageEventView {
    fn from(value: &UsageEvent) -> Self {
        let latency_ms = usage_latency_ms(value);
        Self {
            id: value.event_id.clone(),
            key_id: value.key_id.clone(),
            key_name: value.key_name.clone(),
            account_name: value.account_name.clone(),
            request_method: value.request_method.clone(),
            request_url: value.request_url.clone(),
            latency_ms,
            routing_wait_ms: optional_i64_to_i32(value.timing.routing_wait_ms),
            upstream_headers_ms: optional_i64_to_i32(value.timing.upstream_headers_ms),
            post_headers_body_ms: optional_i64_to_i32(value.timing.post_headers_body_ms),
            request_body_bytes: value.request_body_bytes.and_then(non_negative_i64_to_u64),
            request_body_read_ms: optional_i64_to_i32(value.timing.request_body_read_ms),
            request_json_parse_ms: optional_i64_to_i32(value.timing.request_json_parse_ms),
            pre_handler_ms: optional_i64_to_i32(value.timing.pre_handler_ms),
            first_sse_write_ms: optional_i64_to_i32(value.timing.first_sse_write_ms),
            stream_finish_ms: optional_i64_to_i32(value.timing.stream_finish_ms),
            stream_completed_cleanly: value.stream.stream_completed_cleanly,
            downstream_disconnect: value.stream.downstream_disconnect,
            final_event_type: value.stream.final_event_type.clone(),
            bytes_streamed: value
                .stream
                .bytes_streamed
                .and_then(non_negative_i64_to_u64),
            other_latency_ms: compute_other_latency_ms(
                latency_ms,
                optional_i64_to_i32(value.timing.routing_wait_ms),
                optional_i64_to_i32(value.timing.upstream_headers_ms),
                optional_i64_to_i32(value.timing.post_headers_body_ms),
            ),
            quota_failover_count: value.quota_failover_count,
            routing_diagnostics_json: value.routing_diagnostics_json.clone(),
            endpoint: value.endpoint.clone(),
            model: value.model.clone(),
            status_code: value.status_code.clamp(0, i64::from(i32::MAX)) as i32,
            input_uncached_tokens: non_negative_i64_to_u64(value.input_uncached_tokens)
                .unwrap_or(0),
            input_cached_tokens: non_negative_i64_to_u64(value.input_cached_tokens).unwrap_or(0),
            output_tokens: non_negative_i64_to_u64(value.output_tokens).unwrap_or(0),
            billable_tokens: non_negative_i64_to_u64(value.billable_tokens).unwrap_or(0),
            usage_missing: value.usage_missing,
            credit_usage: value
                .credit_usage
                .as_deref()
                .and_then(|raw| raw.parse().ok()),
            credit_usage_missing: value.credit_usage_missing,
            client_ip: value.client_ip.clone(),
            ip_region: value.ip_region.clone(),
            last_message_content: value.last_message_content.clone(),
            created_at: value.created_at_ms,
        }
    }
}

fn normalize_usage_time_range(
    start_ms: Option<i64>,
    end_ms: Option<i64>,
) -> (Option<i64>, Option<i64>) {
    match (start_ms, end_ms) {
        (Some(start), Some(end)) if end < start => (Some(end), Some(start)),
        values => values,
    }
}

fn normalize_optional_string(value: &str) -> Option<String> {
    let trimmed = value.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn usage_latency_ms(value: &UsageEvent) -> i32 {
    let latency = value.timing.latency_ms.or(value.timing.stream_finish_ms);
    optional_i64_to_i32(latency).unwrap_or(0)
}

fn optional_i64_to_i32(value: Option<i64>) -> Option<i32> {
    value.map(|value| value.clamp(0, i64::from(i32::MAX)) as i32)
}

fn non_negative_i64_to_u64(value: i64) -> Option<u64> {
    (value >= 0).then_some(value as u64)
}

fn compute_other_latency_ms(
    latency_ms: i32,
    routing_wait_ms: Option<i32>,
    upstream_headers_ms: Option<i32>,
    post_headers_body_ms: Option<i32>,
) -> Option<i32> {
    let measured_ms: i64 = [routing_wait_ms, upstream_headers_ms, post_headers_body_ms]
        .into_iter()
        .flatten()
        .map(|value| i64::from(value.max(0)))
        .sum();
    Some((i64::from(latency_ms.max(0)) - measured_ms).clamp(0, i64::from(i32::MAX)) as i32)
}

fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis().min(i64::MAX as u128) as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use llm_access_core::store::UsageEventSource;

    use super::{normalize_usage_query, ListUsageEventsRequest};

    #[test]
    fn normalize_usage_query_accepts_explicit_archive_source() {
        let query = normalize_usage_query(
            ListUsageEventsRequest {
                source: Some("archive".to_string()),
                limit: Some(20),
                offset: Some(0),
                ..ListUsageEventsRequest::default()
            },
            None,
        )
        .expect("archive source should be valid");

        assert_eq!(query.source, UsageEventSource::Archive);
    }

    #[test]
    fn normalize_usage_query_rejects_unknown_source() {
        let err = normalize_usage_query(
            ListUsageEventsRequest {
                source: Some("broad-scan".to_string()),
                limit: Some(20),
                offset: Some(0),
                ..ListUsageEventsRequest::default()
            },
            None,
        )
        .expect_err("unknown usage source should fail");

        assert!(err.contains("source must be one of"));
    }
}
