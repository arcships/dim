//! A local account service and relay for DimOAuth flows: device and browser
//! token grants, refresh rotation, userinfo, the `type=dim` model list, usage
//! and a streaming chat endpoint.
#![allow(dead_code)]
use axum::{
    Json, Router,
    body::Body,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
};

#[derive(Default)]
pub struct Service {
    /// `METHOD path` of every request, with its grant type for tokens.
    pub calls: Mutex<Vec<String>>,
    /// Bearer tokens the chat endpoint answered.
    pub chat_bearers: Mutex<Vec<String>>,
    /// Bearer tokens the relay rejects with 401.
    pub rejected: Mutex<Vec<String>>,
    pub issued: AtomicUsize,
    pub refreshes: AtomicUsize,
    pub reject_refresh: AtomicBool,
    /// `expires_in` of the next issued token only; absent means one hour.
    pub expires_in: Mutex<Option<Value>>,
    pub models: Mutex<Option<Value>>,
}
impl Service {
    pub fn calls(&self, prefix: &str) -> usize {
        self.calls
            .lock()
            .unwrap()
            .iter()
            .filter(|c| c.starts_with(prefix))
            .count()
    }
    fn record(&self, call: String) {
        self.calls.lock().unwrap().push(call);
    }
    fn tokens(&self) -> Value {
        let n = self.issued.fetch_add(1, Ordering::SeqCst);
        let mut body = json!({
            "access_token": format!("access-{n}"),
            "refresh_token": format!("refresh-{n}"),
            "token_type": "Bearer",
            "scope": "openid profile email remote:delegate",
        });
        if let Some(expires) = self.expires_in.lock().unwrap().take() {
            body["expires_in"] = expires;
        }
        body
    }
    fn rejects(&self, headers: &HeaderMap) -> bool {
        let bearer = bearer(headers);
        self.rejected.lock().unwrap().contains(&bearer)
    }
}
fn bearer(headers: &HeaderMap) -> String {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned()
}
fn form(body: &str) -> HashMap<String, String> {
    url::form_urlencoded::parse(body.as_bytes())
        .into_owned()
        .collect()
}
fn unauthorized() -> Response {
    (
        StatusCode::UNAUTHORIZED,
        Json(json!({"error":"invalid_token"})),
    )
        .into_response()
}

pub fn default_models() -> Value {
    json!({"data":[
        {"id":"fixture-model","dim":{"id":"fixture-model","displayName":"Fixture model",
            "endpoint_type":"openai","contextWindow":32000,"maxOutputTokens":128,
            "reasoning":{"supported":true,"effortOptions":["low","high"]}},"zdr":true},
        {"id":"fixture-claude","dim":{"id":"fixture-claude","endpoint_type":"anthropic"}},
        {"id":"fixture-image","dim":{"id":"fixture-image","endpoint_type":"openai",
            "raw":{"modalities":{"input":["text"],"output":["image"]}}}},
        {"id":"fixture-gemini","dim":{"id":"fixture-gemini","endpoint_type":"gemini"}}
    ]})
}

pub struct Fixture {
    pub base: String,
    pub service: Arc<Service>,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub async fn start() -> Fixture {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let service = Arc::new(Service::default());
    let device_base = base.clone();
    let app = Router::new()
        .route(
            "/oauth/device_authorization",
            post(move |State(s): State<Arc<Service>>, body: String| {
                let base = device_base.clone();
                let form = form(&body);
                async move {
                    s.record(format!("POST /oauth/device_authorization {}", form["client_id"]));
                    Json(json!({"device_code":"device-code","user_code":"USER-CODE",
                        "verification_uri":format!("{base}/device"),"expires_in":600,"interval":0}))
                }
            }),
        )
        .route(
            "/oauth/token",
            post(|State(s): State<Arc<Service>>, body: String| async move {
                let form = form(&body);
                let grant = form.get("grant_type").cloned().unwrap_or_default();
                s.record(format!("POST /oauth/token {grant}"));
                match grant.as_str() {
                    "refresh_token" => {
                        s.refreshes.fetch_add(1, Ordering::SeqCst);
                        if s.reject_refresh.load(Ordering::SeqCst) {
                            return (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_grant"})))
                                .into_response();
                        }
                        Json(s.tokens()).into_response()
                    }
                    "authorization_code" if form.get("code").map(String::as_str) == Some("browser-code")
                        && form.contains_key("code_verifier") => Json(s.tokens()).into_response(),
                    "urn:ietf:params:oauth:grant-type:device_code" => Json(s.tokens()).into_response(),
                    _ => (StatusCode::BAD_REQUEST, Json(json!({"error":"invalid_request"}))).into_response(),
                }
            }),
        )
        .route(
            "/oauth/userinfo",
            get(|State(s): State<Arc<Service>>, headers: HeaderMap| async move {
                s.record("GET /oauth/userinfo".into());
                if bearer(&headers).is_empty() {
                    return unauthorized();
                }
                Json(json!({"sub":"fixture-subject","email":" fixture@example.com ","name":"Fixture"}))
                    .into_response()
            }),
        )
        .route(
            "/oauth/revoke",
            post(|State(s): State<Arc<Service>>, body: String| async move {
                let form = form(&body);
                s.record(format!("POST /oauth/revoke {}", form["token"]));
                StatusCode::OK
            }),
        )
        .route(
            "/v1/models",
            get(|State(s): State<Arc<Service>>, headers: HeaderMap| async move {
                s.record("GET /v1/models".into());
                if s.rejects(&headers) {
                    return unauthorized();
                }
                Json(s.models.lock().unwrap().clone().unwrap_or_else(default_models)).into_response()
            }),
        )
        .route(
            "/api/me/usage",
            get(|State(s): State<Arc<Service>>, headers: HeaderMap| async move {
                s.record("GET /api/me/usage".into());
                if s.rejects(&headers) {
                    return unauthorized();
                }
                Json(json!({"success":true,"data":{
                    "subscription":{"product":{"name":"Fixture plan"},"subscription":{"status":"active"},
                        "current_term":{"end_at":"2026-10-28T00:00:00Z"}},
                    "credits":{"subscription_bucket":{"used_units":25000,"total_units":300000,
                        "window_states":[{"window_duration_hours":5,"window_token_used":1200,"window_token_cap":10000},
                            {"window_duration_hours":24}]}},
                    "feature_meters":[{"feature_key":"web_search","total_used":211,"total_allowance":1500,"unit":"call"},
                        {"total_used":1}]}}))
                .into_response()
            }),
        )
        .route(
            "/v1/web-search",
            post(|State(s): State<Arc<Service>>, headers: HeaderMap, Json(body): Json<Value>| async move {
                s.record(format!(
                    "POST /v1/web-search {}:{}",
                    body["query"].as_str().unwrap_or_default(),
                    body["count"].as_u64().unwrap_or_default()
                ));
                if s.rejects(&headers) {
                    return unauthorized();
                }
                Json(json!({"data":{"results":[
                    {"title":"Dim OAuth hit","url":"https://docs.example.com/oauth","snippet":"Account search snippet","publishedAt":"2026-10-28T00:00:00Z"},
                    {"title":"Other hit","url":"https://other.test/","description":"Fallback description"}
                ]}}))
                .into_response()
            }),
        )
        .route(
            "/v1/chat/completions",
            post(|State(s): State<Arc<Service>>, headers: HeaderMap| async move {
                s.record("POST /v1/chat/completions".into());
                s.chat_bearers.lock().unwrap().push(bearer(&headers));
                if s.rejects(&headers) {
                    return unauthorized();
                }
                Response::builder()
                    .header("content-type", "text/event-stream")
                    .body(Body::from(concat!(
                        "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{\"content\":\"managed reply\"},\"finish_reason\":null}]}\n\n",
                        "data: {\"id\":\"fixture\",\"choices\":[{\"index\":0,\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
                        "data: [DONE]\n\n"
                    )))
                    .unwrap()
            }),
        )
        .with_state(service.clone());
    let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    Fixture {
        base,
        service,
        task,
    }
}
