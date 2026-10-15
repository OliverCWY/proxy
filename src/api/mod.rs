pub mod webui;

use std::{collections::HashMap, sync::Arc, time::Duration};
use axum::{
    Json, Router,
    extract::{Path, Query, State},
    http::StatusCode,
    routing::{get, post, put},
};
use serde::{Deserialize, Serialize};
use tower_http::cors::CorsLayer;

use crate::{
    outbound::OutboundConnector,
    pool::{NodeHealthStatus, ProxyPool},
    protocol::TargetAddr,
    reverse::rules::{AccountMatcherRuntime, AccountRule},
};

#[derive(Clone)]
pub struct AppState {
    pub pool: Arc<ProxyPool>,
    pub rules_runtime: Arc<AccountMatcherRuntime>,
    pub security: Arc<crate::security::SecurityFilter>,
    pub admin_token: Option<String>,
    pub proxy_port: u16,
}

pub fn create_api_router(state: AppState) -> Router {
    let api_v1 = Router::new()
        .route("/system/info", get(get_system_info))
        .route("/nodes", get(list_nodes))
        .route("/nodes/{id}/actions/probe", post(probe_node))
        .route("/nodes/{id}/actions/block", post(block_node))
        .route("/nodes/{id}/actions/unblock", post(unblock_node))
        .route("/security/blacklist", get(list_blacklist).post(add_blacklist))
        .route("/security/blacklist/{domain}", axum::routing::delete(remove_blacklist))
        .route("/rules", get(list_rules))
        .route("/rules/{prefix}", put(upsert_rule).delete(delete_rule))
        .route("/rules:resolve", post(resolve_rule))
        .route("/subscriptions/sync", post(sync_subscription))
        .route_layer(axum::middleware::from_fn_with_state(state.clone(), admin_auth_middleware));

    Router::new()
        .route("/", get(webui::root_redirect))
        .route("/ui", get(webui::ui_root_redirect))
        .route("/ui/", get(webui::static_handler))
        .route("/ui/{*path}", get(webui::static_handler))
        .route("/metrics", get(prometheus_metrics))
        .nest("/api/v1", api_v1)
        .layer(CorsLayer::permissive())
        .with_state(state)
}

async fn admin_auth_middleware(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Result<axum::response::Response, StatusCode> {
    if let Some(expected) = &state.admin_token {
        let mut authenticated = false;
        // 1. Check Authorization: Bearer <token>
        if let Some(auth_val) = req.headers().get(axum::http::header::AUTHORIZATION) {
            if let Ok(auth_str) = auth_val.to_str() {
                if let Some(token) = auth_str.strip_prefix("Bearer ") {
                    if token.trim() == expected {
                        authenticated = true;
                    }
                }
            }
        }
        // 2. Check query param ?token=<token>
        if !authenticated {
            if let Some(query) = req.uri().query() {
                for pair in query.split('&') {
                    if let Some(token) = pair.strip_prefix("token=") {
                        if token.trim() == expected {
                            authenticated = true;
                            break;
                        }
                    }
                }
            }
        }
        if !authenticated {
            return Err(StatusCode::UNAUTHORIZED);
        }
    }
    Ok(next.run(req).await)
}

/// Serve a single pre-buffered HTTP connection with the Axum router
pub async fn serve_single_connection(
    stream: crate::inbound::prefixed::PrefixedStream<tokio::net::TcpStream>,
    router: Router,
) {
    use hyper_util::rt::{TokioExecutor, TokioIo};
    use hyper_util::server::conn::auto::Builder;
    use tower::Service;

    let io = TokioIo::new(stream);
    let mut make_service = router.into_make_service();
    let service = match make_service.call(()).await {
        Ok(s) => s,
        Err(_) => return,
    };
    let hyper_service = hyper_util::service::TowerToHyperService::new(service);

    let _ = Builder::new(TokioExecutor::new())
        .serve_connection(io, hyper_service)
        .await;
}

// 1. System Info
#[derive(Serialize)]
pub struct SystemInfoResponse {
    pub version: &'static str,
    pub status: &'static str,
    pub anonymous_allowed: bool,
    pub total_nodes: usize,
    pub active_leases: usize,
    pub proxy_port: u16,
}

async fn get_system_info(State(state): State<AppState>) -> Json<SystemInfoResponse> {
    Json(SystemInfoResponse {
        version: env!("CARGO_PKG_VERSION"),
        status: "healthy",
        anonymous_allowed: state.admin_token.is_none(),
        total_nodes: state.pool.node_count(),
        active_leases: state.pool.leases().active_count(),
        proxy_port: state.proxy_port,
    })
}

// 2. Nodes List & Pagination
#[derive(Deserialize)]
pub struct NodeFilterQuery {
    pub limit: Option<usize>,
    pub offset: Option<usize>,
    pub status: Option<String>, // "healthy" | "cooldown" | "all"
    pub search: Option<String>,
}

#[derive(Serialize)]
pub struct NodeSummary {
    pub id: usize,
    pub tag: String,
    pub status: &'static str,
    pub dial_addrs: Vec<String>,
    pub total_requests: u64,
    pub successful_requests: u64,
    pub consecutive_failures: usize,
    pub active_connections: usize,
    pub latency_ewma_ms: u64,
}

#[derive(Serialize)]
pub struct NodeListResponse {
    pub items: Vec<NodeSummary>,
    pub total: usize,
    pub healthy_count: usize,
    pub cooldown_count: usize,
}

async fn list_nodes(
    State(state): State<AppState>,
    Query(filters): Query<NodeFilterQuery>,
) -> Json<NodeListResponse> {
    // Collect all nodes from pool snapshot directly in O(1)
    let all_nodes = state.pool.all_nodes();

    let mut healthy_count = 0;
    let mut cooldown_count = 0;

    for n in all_nodes.iter() {
        match n.status() {
            NodeHealthStatus::Healthy | NodeHealthStatus::HalfOpen => healthy_count += 1,
            NodeHealthStatus::Cooldown | NodeHealthStatus::Blocked => cooldown_count += 1,
        }
    }

    let mut filtered = all_nodes
        .iter()
        .cloned()
        .filter(|n| {
            if let Some(status_filter) = &filters.status {
                match status_filter.as_str() {
                    "healthy" => n.is_available(),
                    "cooldown" => !n.is_available(),
                    _ => true,
                }
            } else {
                true
            }
        })
        .filter(|n| {
            if let Some(kw) = &filters.search {
                let kw = kw.to_lowercase();
                n.tag.to_lowercase().contains(&kw)
            } else {
                true
            }
        })
        .collect::<Vec<_>>();

    let total = filtered.len();
    let offset = filters.offset.unwrap_or(0);
    let limit = filters.limit.unwrap_or(50);

    let page_items = if offset < filtered.len() {
        filtered.drain(offset..).take(limit).collect()
    } else {
        vec![]
    };

    let items = page_items
        .into_iter()
        .map(|n| NodeSummary {
            id: n.id,
            tag: n.tag.clone(),
            status: match n.status() {
                NodeHealthStatus::Healthy => "healthy",
                NodeHealthStatus::HalfOpen => "half_open",
                NodeHealthStatus::Cooldown => "cooldown",
                NodeHealthStatus::Blocked => "blocked",
            },
            dial_addrs: n.config.dial_addrs.iter().map(|a| a.to_string()).collect(),
            total_requests: n.stats.total_requests.load(std::sync::atomic::Ordering::Relaxed),
            successful_requests: n.stats.successful_requests.load(std::sync::atomic::Ordering::Relaxed),
            consecutive_failures: n.stats.consecutive_failures.load(std::sync::atomic::Ordering::Relaxed),
            active_connections: n.stats.active_connections.load(std::sync::atomic::Ordering::Relaxed),
            latency_ewma_ms: n.stats.latency_ewma_ms.load(std::sync::atomic::Ordering::Relaxed),
        })
        .collect();

    Json(NodeListResponse {
        items,
        total,
        healthy_count,
        cooldown_count,
    })
}

// 3. Probe Single Node
#[derive(Serialize)]
pub struct ProbeResponse {
    pub success: bool,
    pub latency_ms: Option<u64>,
    pub error: Option<String>,
}

async fn probe_node(
    State(state): State<AppState>,
    Path(id): Path<usize>,
) -> Result<Json<ProbeResponse>, StatusCode> {
    let node = state.pool.find_node(id).ok_or(StatusCode::NOT_FOUND)?;

    let target = TargetAddr::from_domain("cp.cloudflare.com", 80);
    let start = std::time::Instant::now();
    let res = OutboundConnector::connect(&node.config, &target, Duration::from_secs(5)).await;
    let latency_ms = start.elapsed().as_millis() as u64;

    match res {
        Ok(_) => {
            node.mark_success();
            Ok(Json(ProbeResponse {
                success: true,
                latency_ms: Some(latency_ms),
                error: None,
            }))
        }
        Err(err) => {
            node.mark_failure(3, Duration::from_secs(10));
            Ok(Json(ProbeResponse {
                success: false,
                latency_ms: None,
                error: Some(err.to_string()),
            }))
        }
    }
}

// 4. Rules & Resolve Sandbox
#[derive(Serialize, Deserialize)]
pub struct RulePayload {
    pub headers: Vec<String>,
}

async fn list_rules(State(state): State<AppState>) -> Json<Vec<AccountRule>> {
    Json(state.rules_runtime.get_rules())
}

async fn upsert_rule(
    State(state): State<AppState>,
    Path(prefix): Path<String>,
    Json(payload): Json<RulePayload>,
) -> StatusCode {
    let rule = AccountRule {
        url_prefix: prefix,
        headers: payload.headers,
    };
    state.rules_runtime.upsert_rule(rule);
    StatusCode::OK
}

async fn delete_rule(
    State(state): State<AppState>,
    Path(prefix): Path<String>,
) -> StatusCode {
    if prefix == "*" {
        return StatusCode::FORBIDDEN; // Wildcard cannot be deleted
    }
    if state.rules_runtime.delete_rule(&prefix) {
        StatusCode::OK
    } else {
        StatusCode::NOT_FOUND
    }
}

#[derive(Deserialize)]
pub struct ResolveRuleRequest {
    pub url: String,
    pub headers: Option<HashMap<String, String>>,
}

#[derive(Serialize)]
pub struct ResolveRuleResponse {
    pub matched_prefix: Option<String>,
    pub headers_to_inspect: Vec<String>,
    pub extracted_account: Option<String>,
}

async fn resolve_rule(
    State(state): State<AppState>,
    Json(req): Json<ResolveRuleRequest>,
) -> Result<Json<ResolveRuleResponse>, (StatusCode, String)> {
    let matcher = state.rules_runtime.load();
    let (host, path) = parse_url_host_path(&req.url)
        .ok_or_else(|| (StatusCode::BAD_REQUEST, "Invalid URL format".into()))?;

    let matched_headers = matcher.match_url(&host, &path).map(|h| h.to_vec()).unwrap_or_default();

    let mut extracted_account = None;
    if let Some(req_headers) = &req.headers {
        for h_name in &matched_headers {
            for (k, v) in req_headers {
                if k.eq_ignore_ascii_case(h_name) && !v.trim().is_empty() {
                    extracted_account = Some(v.trim().to_string());
                    break;
                }
            }
            if extracted_account.is_some() {
                break;
            }
        }
    }

    Ok(Json(ResolveRuleResponse {
        matched_prefix: Some(format!("{host}{path}")),
        headers_to_inspect: matched_headers,
        extracted_account,
    }))
}

fn parse_url_host_path(url_str: &str) -> Option<(String, String)> {
    let trimmed = url_str.trim();
    let without_scheme = if let Some(stripped) = trimmed.strip_prefix("https://") {
        stripped
    } else if let Some(stripped) = trimmed.strip_prefix("http://") {
        stripped
    } else {
        trimmed
    };

    let (host, path) = match without_scheme.find('/') {
        Some(idx) => (&without_scheme[..idx], &without_scheme[idx..]),
        None => (without_scheme, "/"),
    };

    if host.is_empty() {
        None
    } else {
        Some((host.to_string(), path.to_string()))
    }
}

// 5. Subscription Sync
#[derive(Deserialize)]
pub struct SyncSubscriptionRequest {
    pub url: Option<String>,
    pub content: Option<String>,
}

#[derive(Serialize)]
pub struct SyncSubscriptionResponse {
    pub imported_count: usize,
    pub total_nodes: usize,
}

async fn sync_subscription(
    State(state): State<AppState>,
    Json(req): Json<SyncSubscriptionRequest>,
) -> Result<Json<SyncSubscriptionResponse>, (StatusCode, String)> {
    let new_nodes = if let Some(raw_text) = req.content {
        crate::subscription::SubscriptionParser::parse_subscription_text(&raw_text).await
    } else if let Some(url) = req.url {
        crate::subscription::SubscriptionParser::fetch_and_parse(state.pool.get_reqwest_client(), &url)
            .await
            .map_err(|e| (StatusCode::BAD_REQUEST, format!("Fetch failed: {e}")))?
    } else {
        return Err((StatusCode::BAD_REQUEST, "Must provide url or content".into()));
    };

    let imported_count = new_nodes.len();
    state.pool.reload_nodes(new_nodes);

    Ok(Json(SyncSubscriptionResponse {
        imported_count,
        total_nodes: state.pool.node_count(),
    }))
}

// 6. Prometheus Metrics Endpoint
async fn prometheus_metrics(State(state): State<AppState>) -> (axum::http::HeaderMap, String) {
    let mut headers = axum::http::HeaderMap::new();
    headers.insert(
        axum::http::header::CONTENT_TYPE,
        axum::http::HeaderValue::from_static("text/plain; version=0.0.4; charset=utf-8"),
    );

    let all_nodes = state.pool.all_nodes();

    let total_nodes = all_nodes.len();
    let mut healthy_nodes = 0;
    let mut cooldown_nodes = 0;
    for n in all_nodes.iter() {
        match n.status() {
            NodeHealthStatus::Healthy | NodeHealthStatus::HalfOpen => healthy_nodes += 1,
            NodeHealthStatus::Cooldown | NodeHealthStatus::Blocked => cooldown_nodes += 1,
        }
    }
    let active_leases = state.pool.leases().active_count();

    let mut out = String::with_capacity(2048);
    out.push_str("# HELP proxy_nodes_total Total number of proxy nodes\n");
    out.push_str("# TYPE proxy_nodes_total gauge\n");
    out.push_str(&format!("proxy_nodes_total {total_nodes}\n\n"));

    out.push_str("# HELP proxy_nodes_healthy Number of healthy/available proxy nodes\n");
    out.push_str("# TYPE proxy_nodes_healthy gauge\n");
    out.push_str(&format!("proxy_nodes_healthy {healthy_nodes}\n\n"));

    out.push_str("# HELP proxy_nodes_cooldown Number of cooling down proxy nodes\n");
    out.push_str("# TYPE proxy_nodes_cooldown gauge\n");
    out.push_str(&format!("proxy_nodes_cooldown {cooldown_nodes}\n\n"));

    out.push_str("# HELP proxy_sticky_leases_active Number of active sticky session leases\n");
    out.push_str("# TYPE proxy_sticky_leases_active gauge\n");
    out.push_str(&format!("proxy_sticky_leases_active {active_leases}\n\n"));

    out.push_str("# HELP proxy_node_latency_ewma_ms Smoothed TD-EWMA latency in milliseconds\n");
    out.push_str("# TYPE proxy_node_latency_ewma_ms gauge\n");
    for n in all_nodes.iter() {
        let ewma = n.stats.latency_ewma_ms.load(std::sync::atomic::Ordering::Relaxed);
        out.push_str(&format!("proxy_node_latency_ewma_ms{{id=\"{}\",tag=\"{}\"}} {ewma}\n", n.id, n.tag));
    }
    out.push('\n');

    out.push_str("# HELP proxy_node_requests_total Total requests routed to node\n");
    out.push_str("# TYPE proxy_node_requests_total counter\n");
    for n in all_nodes.iter() {
        let reqs = n.stats.total_requests.load(std::sync::atomic::Ordering::Relaxed);
        out.push_str(&format!("proxy_node_requests_total{{id=\"{}\",tag=\"{}\"}} {reqs}\n", n.id, n.tag));
    }
    out.push('\n');

    out.push_str("# HELP proxy_node_active_connections Number of active connections on node\n");
    out.push_str("# TYPE proxy_node_active_connections gauge\n");
    for n in all_nodes.iter() {
        let conns = n.stats.active_connections.load(std::sync::atomic::Ordering::Relaxed);
        out.push_str(&format!("proxy_node_active_connections{{id=\"{}\",tag=\"{}\"}} {conns}\n", n.id, n.tag));
    }

    (headers, out)
}

// 7. Node Block / Unblock Actions
async fn block_node(
    State(state): State<AppState>,
    Path(id): Path<usize>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if let Some(node) = state.pool.find_node(id) {
        node.block();
        Ok(Json(serde_json::json!({ "success": true, "id": id, "status": "blocked" })))
    } else {
        Err((StatusCode::NOT_FOUND, "Node not found".into()))
    }
}

async fn unblock_node(
    State(state): State<AppState>,
    Path(id): Path<usize>,
) -> Result<Json<serde_json::Value>, (StatusCode, String)> {
    if let Some(node) = state.pool.find_node(id) {
        node.unblock();
        Ok(Json(serde_json::json!({ "success": true, "id": id, "status": "healthy" })))
    } else {
        Err((StatusCode::NOT_FOUND, "Node not found".into()))
    }
}

// 8. Security Blacklist APIs
#[derive(Deserialize)]
pub struct AddBlacklistRequest {
    pub domain: String,
}

async fn list_blacklist(State(state): State<AppState>) -> Json<Vec<String>> {
    Json(state.security.list_blocked_domains())
}

async fn add_blacklist(
    State(state): State<AppState>,
    Json(req): Json<AddBlacklistRequest>,
) -> (StatusCode, Json<serde_json::Value>) {
    let domain = req.domain.trim().to_string();
    if domain.is_empty() {
        return (StatusCode::BAD_REQUEST, Json(serde_json::json!({ "error": "Domain cannot be empty" })));
    }
    state.security.add_blocked_domain(&domain);
    (StatusCode::OK, Json(serde_json::json!({ "success": true, "domain": domain })))
}

async fn remove_blacklist(
    State(state): State<AppState>,
    Path(domain): Path<String>,
) -> (StatusCode, Json<serde_json::Value>) {
    let removed = state.security.remove_blocked_domain(&domain);
    (StatusCode::OK, Json(serde_json::json!({ "success": removed, "domain": domain })))
}
