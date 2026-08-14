use crate::{mcp, serial::SerialHub};
use axum::{
    Json, Router,
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode, header},
    response::{IntoResponse, Response},
    routing::post,
};
use serde_json::{Value, json};
use std::sync::{Arc, Mutex};

pub const MCP_ENDPOINT: &str = "/mcp";
const PROTOCOL_VERSION_HEADER: &str = "mcp-protocol-version";

#[derive(Clone)]
struct AppState {
    hub: Arc<Mutex<SerialHub>>,
}

pub fn router() -> Router {
    let state = AppState {
        hub: Arc::new(Mutex::new(SerialHub::new())),
    };
    Router::new()
        .route(MCP_ENDPOINT, post(handle_post))
        .with_state(state)
}

async fn handle_post(State(state): State<AppState>, headers: HeaderMap, body: Bytes) -> Response {
    if headers.contains_key(header::ORIGIN) {
        return (
            StatusCode::FORBIDDEN,
            "browser origins are not allowed by this local MCP server",
        )
            .into_response();
    }
    if !header_supports(&headers, header::CONTENT_TYPE, "application/json") {
        return (
            StatusCode::UNSUPPORTED_MEDIA_TYPE,
            "Content-Type must be application/json",
        )
            .into_response();
    }
    if !header_supports(&headers, header::ACCEPT, "application/json")
        || !header_supports(&headers, header::ACCEPT, "text/event-stream")
    {
        return (
            StatusCode::NOT_ACCEPTABLE,
            "Accept must include application/json and text/event-stream",
        )
            .into_response();
    }
    if !protocol_version_is_supported(&headers) {
        return (StatusCode::BAD_REQUEST, "unsupported MCP-Protocol-Version").into_response();
    }

    let message = match serde_json::from_slice::<Value>(&body) {
        Ok(message) => message,
        Err(_) => {
            return json_rpc_error(StatusCode::BAD_REQUEST, Value::Null, -32700, "parse error");
        }
    };

    if is_json_rpc_response(&message) {
        return StatusCode::ACCEPTED.into_response();
    }

    let response_id = message.get("id").cloned().unwrap_or(Value::Null);
    let result = tokio::task::spawn_blocking(move || {
        let mut hub = state
            .hub
            .lock()
            .map_err(|_| "serial hub lock poisoned".to_string())?;
        Ok::<_, String>(mcp::handle_message(message, &mut hub))
    })
    .await;

    match result {
        Ok(Ok(Some(response))) => Json(response).into_response(),
        Ok(Ok(None)) => StatusCode::ACCEPTED.into_response(),
        Ok(Err(message)) => json_rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            response_id,
            -32603,
            &message,
        ),
        Err(error) => json_rpc_error(
            StatusCode::INTERNAL_SERVER_ERROR,
            response_id,
            -32603,
            &format!("request task failed: {error}"),
        ),
    }
}

fn header_supports(headers: &HeaderMap, name: header::HeaderName, expected: &str) -> bool {
    headers.get_all(name).iter().any(|value| {
        value.to_str().is_ok_and(|value| {
            value.split(',').any(|media_type| {
                media_type
                    .split(';')
                    .next()
                    .is_some_and(|value| value.trim().eq_ignore_ascii_case(expected))
            })
        })
    })
}

fn protocol_version_is_supported(headers: &HeaderMap) -> bool {
    headers.get(PROTOCOL_VERSION_HEADER).is_none_or(|value| {
        value
            .to_str()
            .is_ok_and(|value| mcp::SUPPORTED_PROTOCOL_VERSIONS.contains(&value.trim()))
    })
}

fn is_json_rpc_response(message: &Value) -> bool {
    message.as_object().is_some_and(|message| {
        message.get("jsonrpc").and_then(Value::as_str) == Some("2.0")
            && message.contains_key("id")
            && !message.contains_key("method")
            && (message.contains_key("result") ^ message.contains_key("error"))
    })
}

fn json_rpc_error(status: StatusCode, id: Value, code: i64, message: &str) -> Response {
    (
        status,
        Json(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": code, "message": message}
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::{Body, to_bytes},
        http::Request,
    };
    use tower::ServiceExt;

    fn post_request(body: impl Into<Body>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri(MCP_ENDPOINT)
            .header(header::CONTENT_TYPE, "application/json")
            .header(header::ACCEPT, "application/json, text/event-stream")
            .body(body.into())
            .unwrap()
    }

    fn initialize_message() -> Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "test-client", "version": "1.0.0"}
            }
        })
    }

    #[tokio::test]
    async fn initialize_returns_json_response() {
        let response = router()
            .oneshot(post_request(initialize_message().to_string()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.headers()[header::CONTENT_TYPE], "application/json");
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(body["result"]["serverInfo"]["name"], "serial-mcp");
    }

    #[tokio::test]
    async fn notification_returns_accepted_without_body() {
        let message = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized",
            "params": {}
        });
        let response = router()
            .oneshot(post_request(message.to_string()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn client_response_returns_accepted_without_body() {
        let message = json!({"jsonrpc": "2.0", "id": 7, "result": {}});
        let response = router()
            .oneshot(post_request(message.to_string()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::ACCEPTED);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert!(body.is_empty());
    }

    #[tokio::test]
    async fn get_returns_method_not_allowed() {
        let response = router()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(MCP_ENDPOINT)
                    .header(header::ACCEPT, "text/event-stream")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    }

    #[tokio::test]
    async fn browser_origin_is_rejected() {
        let mut request = post_request(initialize_message().to_string());
        request
            .headers_mut()
            .insert(header::ORIGIN, "https://example.com".parse().unwrap());
        let response = router().oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn unsupported_protocol_version_is_rejected() {
        let mut request = post_request(initialize_message().to_string());
        request
            .headers_mut()
            .insert(PROTOCOL_VERSION_HEADER, "2024-11-05".parse().unwrap());
        let response = router().oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn invalid_json_returns_parse_error() {
        let response = router().oneshot(post_request("not json")).await.unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let body: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(body["error"]["code"], -32700);
    }
}
