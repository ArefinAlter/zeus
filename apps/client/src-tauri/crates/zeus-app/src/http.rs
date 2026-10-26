//! A loopback HTTP front of the route table, for testing the interface in an ordinary browser.
//!
//! It is the same closed table the desktop window calls, behind a bearer token and an origin allow-list, bound to
//! 127.0.0.1. It exists so the interface can be driven by Playwright against the real engine; the installer does
//! not ship it and the desktop app does not start it.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::Router;
use axum::body::to_bytes;
use axum::extract::{Request, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::App;
use crate::failure::Failure;

#[derive(Clone)]
struct Shared {
    app: Arc<App>,
    token: String,
    origins: Vec<String>,
}

fn cors(headers: &mut HeaderMap, origin: Option<&str>, shared: &Shared) {
    let loopback = |o: &str| {
        ["http://127.0.0.1:", "http://localhost:"].iter().any(|p| {
            o.strip_prefix(p)
                .is_some_and(|port| !port.is_empty() && port.bytes().all(|b| b.is_ascii_digit()))
        })
    };
    if let Some(o) = origin.filter(|o| loopback(o) || shared.origins.iter().any(|x| x == o))
        && let Ok(v) = HeaderValue::from_str(o)
    {
        headers.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        headers.insert(header::VARY, HeaderValue::from_static("Origin"));
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static("authorization, content-type, x-zeus-client"),
        );
        headers.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, DELETE, OPTIONS"),
        );
    }
}

async fn handle(State(shared): State<Shared>, request: Request) -> Response {
    let origin = request
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let mut response = respond(&shared, request).await;
    cors(response.headers_mut(), origin.as_deref(), &shared);
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

async fn respond(shared: &Shared, request: Request) -> Response {
    if request.method() == Method::OPTIONS {
        return StatusCode::NO_CONTENT.into_response();
    }
    let given = request
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::trim)
        .unwrap_or_default()
        .to_owned();
    if given != shared.token {
        return fail(
            StatusCode::UNAUTHORIZED,
            &Failure::new("app.token_required", "Paste the harness token."),
        );
    }
    if request.uri().path() == "/__harness/shutdown" {
        // Test harness only: stop the engine cleanly (zeusd included), then end the process.
        shared.app.shutdown().await;
        tokio::spawn(async {
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            std::process::exit(0);
        });
        return axum::Json(json!({"ok": true})).into_response();
    }
    let route = format!("{} {}", request.method(), request.uri().path());
    let bytes = match to_bytes(request.into_body(), 4 * 1024 * 1024).await {
        Ok(b) => b,
        Err(_) => {
            return fail(
                StatusCode::PAYLOAD_TOO_LARGE,
                &Failure::new("app.too_large", "Request too large."),
            );
        }
    };
    let body: Value = if bytes.is_empty() {
        Value::Null
    } else {
        match serde_json::from_slice(&bytes) {
            Ok(v) => v,
            Err(_) => return fail(StatusCode::BAD_REQUEST, &Failure::invalid("Not JSON.")),
        }
    };
    match shared.app.call(&route, body).await {
        Ok(v) => axum::Json(v).into_response(),
        Err(f) => {
            let status = if f.code.starts_with("engine.") {
                StatusCode::SERVICE_UNAVAILABLE
            } else if f.code == "app.unknown_route" {
                StatusCode::NOT_FOUND
            } else {
                StatusCode::BAD_REQUEST
            };
            fail(status, &f)
        }
    }
}

fn fail(status: StatusCode, f: &Failure) -> Response {
    (status, axum::Json(json!({"error": f}))).into_response()
}

/// Serve `app` on `addr` (loopback only) until the returned future is dropped. Returns the bound address.
pub async fn serve(
    app: Arc<App>,
    addr: SocketAddr,
    token: String,
    origins: Vec<String>,
) -> std::io::Result<SocketAddr> {
    if !addr.ip().is_loopback() {
        return Err(std::io::Error::other(
            "the harness listens on loopback only",
        ));
    }
    let shared = Shared {
        app,
        token,
        origins,
    };
    let router = Router::new().fallback(handle).with_state(shared);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    let bound = listener.local_addr()?;
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    Ok(bound)
}
