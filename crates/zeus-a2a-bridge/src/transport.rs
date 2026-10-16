//! Deliberately bounded HTTP/1.1 subset: one Content-Length request per connection, no proxying,
//! transfer coding, upgrade or keep-alive. Authenticate before reading a request body.
use crate::Bridge;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc, time::Duration};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::sync::Semaphore;
use zeus_api::{ApiError, ApiResult};

const MAX_HEADER: usize = 16_384;
const MAX_BODY: usize = 1_048_576;

/// Bind the optional loopback bridge. Dropping this future closes active client connections.
pub async fn serve(bridge: Arc<Bridge>) -> ApiResult<()> {
    bridge.config.validate()?;
    let listener = tokio::net::TcpListener::bind(bridge.config.bind)
        .await
        .map_err(|_| ApiError::new("api.unavailable", "A2A loopback listener could not bind"))?;
    let permits = Arc::new(Semaphore::new(32));
    let mut connections = tokio::task::JoinSet::new();
    loop {
        tokio::select! {
            next = listener.accept() => {
                let (stream, address) = next.map_err(|_|ApiError::new("api.unavailable","A2A listener failed"))?;
                if !address.ip().is_loopback() { continue; }
                let Ok(permit) = permits.clone().try_acquire_owned() else { continue; };
                let bridge = bridge.clone();
                connections.spawn(async move {
                    let _permit = permit;
                    let _ = connection(stream, &bridge).await;
                });
            },
            _ = connections.join_next(), if !connections.is_empty() => {},
        }
    }
}

struct Head {
    method: String,
    path: String,
    headers: BTreeMap<String, String>,
}
fn parse_head(bytes: &[u8]) -> Option<Head> {
    let text = std::str::from_utf8(bytes).ok()?;
    if !text.is_ascii() {
        return None;
    }
    let mut lines = text.split("\r\n");
    let request: Vec<_> = lines.next()?.split(' ').collect();
    if request.len() != 3
        || request[2] != "HTTP/1.1"
        || !matches!(request[0], "GET" | "POST")
        || !request[1].starts_with('/')
    {
        return None;
    }
    let mut headers = BTreeMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if headers.len() >= 32 {
            return None;
        }
        let (key, value) = line.split_once(':')?;
        if key.is_empty()
            || !key.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
            || value.bytes().any(|b| b < 0x20 && b != b'\t')
        {
            return None;
        }
        if headers
            .insert(key.to_ascii_lowercase(), value.trim().to_owned())
            .is_some()
        {
            return None;
        }
    }
    Some(Head {
        method: request[0].into(),
        path: request[1].into(),
        headers,
    })
}

pub(crate) async fn connection<S: AsyncRead + AsyncWrite + Unpin>(
    mut stream: S,
    bridge: &Bridge,
) -> std::io::Result<()> {
    let header = tokio::time::timeout(Duration::from_secs(5), async {
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            if bytes.len() >= MAX_HEADER {
                return Err(std::io::Error::other("header limit"));
            }
            bytes.push(stream.read_u8().await?);
        }
        Ok(bytes)
    })
    .await;
    let head = match header {
        Ok(Ok(bytes)) => parse_head(&bytes),
        _ => None,
    };
    let Some(head) = head else {
        return respond(
            &mut stream,
            400,
            json!({"error":"Invalid or timed-out HTTP headers"}),
        )
        .await;
    };
    let headers = &head.headers;
    if headers.get("host") != Some(&bridge.config.bind.to_string())
        || headers.contains_key("forwarded")
        || headers.contains_key("x-forwarded-host")
        || headers
            .get("origin")
            .is_some_and(|origin| !bridge.config.allowed_origins.contains(origin))
    {
        return respond(&mut stream, 403, json!({"error":"Host or Origin refused"})).await;
    }
    if headers.contains_key("transfer-encoding")
        || headers.contains_key("upgrade")
        || headers.contains_key("expect")
    {
        return respond(
            &mut stream,
            400,
            json!({"error":"Unsupported HTTP framing"}),
        )
        .await;
    }
    if head.method == "GET" && head.path == zeus_a2a::WELL_KNOWN_PATH {
        return respond(&mut stream, 200, bridge.card()).await;
    }
    if head.method != "POST" || head.path != "/a2a" {
        return respond(&mut stream, 404, json!({"error":"Not found"})).await;
    }
    let identity = headers
        .get("authorization")
        .and_then(|s| s.strip_prefix("Bearer "))
        .and_then(|token| bridge.config.authenticate(token));
    let Some(peer) = identity else {
        return respond(
            &mut stream,
            401,
            json!({"error":"Bearer authentication required"}),
        )
        .await;
    };
    if headers.get("a2a-version").is_some_and(|s| s != "1.0") {
        return respond(
            &mut stream,
            400,
            json!({"error":"Only A2A version 1.0 is supported"}),
        )
        .await;
    }
    if !headers.get("content-type").is_some_and(|s| {
        matches!(
            s.split(';').next(),
            Some("application/json" | "application/a2a+json")
        )
    }) {
        return respond(
            &mut stream,
            415,
            json!({"error":"JSON content type required"}),
        )
        .await;
    }
    let length = headers
        .get("content-length")
        .filter(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
        .and_then(|s| s.parse::<usize>().ok());
    let Some(length) = length else {
        return respond(&mut stream, 411, json!({"error":"Content-Length required"})).await;
    };
    if length == 0 || length > MAX_BODY {
        return respond(&mut stream, 413, json!({"error":"Body exceeds limit"})).await;
    }
    let mut bytes = vec![0u8; length];
    if !matches!(
        tokio::time::timeout(Duration::from_secs(10), stream.read_exact(&mut bytes)).await,
        Ok(Ok(_))
    ) {
        return respond(
            &mut stream,
            408,
            json!({"error":"Request body timed out or ended early"}),
        )
        .await;
    }
    let input = match serde_json::from_slice(&bytes) {
        Ok(input) => input,
        Err(_) => {
            return respond(
                &mut stream,
                200,
                crate::rpc::rpc_error(Value::Null, -32700, "Invalid JSON payload"),
            )
            .await;
        }
    };
    let result = tokio::time::timeout(Duration::from_secs(95), bridge.rpc(&peer, input)).await;
    match result {
        Ok(value) => respond(&mut stream, 200, value).await,
        Err(_) => {
            respond(
                &mut stream,
                503,
                json!({"error":"Operation timed out; retry with the same idempotency key"}),
            )
            .await
        }
    }
}

async fn respond<S: AsyncWrite + Unpin>(
    stream: &mut S,
    status: u16,
    value: Value,
) -> std::io::Result<()> {
    let bytes = serde_json::to_vec(&value).map_err(std::io::Error::other)?;
    let (status, bytes) = if bytes.len() > MAX_BODY {
        (
            503,
            br#"{"error":"Response exceeds limit; request a smaller page"}"#.to_vec(),
        )
    } else {
        (status, bytes)
    };
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        408 => "Request Timeout",
        411 => "Length Required",
        413 => "Content Too Large",
        415 => "Unsupported Media Type",
        _ => "Service Unavailable",
    };
    let challenge = if status == 401 {
        "WWW-Authenticate: Bearer\r\n"
    } else {
        ""
    };
    let header = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/a2a+json\r\nContent-Length: {}\r\nA2A-Version: 1.0\r\nConnection: close\r\nCache-Control: no-store\r\nX-Content-Type-Options: nosniff\r\n{challenge}\r\n",
        bytes.len()
    );
    tokio::time::timeout(Duration::from_secs(5), async {
        stream.write_all(header.as_bytes()).await?;
        stream.write_all(&bytes).await?;
        stream.shutdown().await
    })
    .await
    .map_err(|_| std::io::Error::other("response timeout"))?
}
