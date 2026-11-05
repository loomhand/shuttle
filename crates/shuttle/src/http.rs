//! The MCP server over Streamable HTTP: `POST /mcp` takes one JSON-RPC message and answers it as JSON, so an agent on the network can drive this desktop.

use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, Full, LengthLimitError, Limited};
use hyper::body::{Bytes, Incoming};
use hyper::header::{ALLOW, AUTHORIZATION, CONTENT_TYPE, HeaderValue, ORIGIN, WWW_AUTHENTICATE};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::rt::TokioIo;
use serde_json::Value;
use tokio::net::TcpListener;
use tokio::sync::Semaphore;

use crate::plugins::Plugins;
use crate::{log, mcp};

/// The environment variable that holds the bearer token every HTTP request must carry.
pub const TOKEN_ENV: &str = "SHUTTLE_MCP_TOKEN";

const PATH: &str = "/mcp";
const MAX_BODY: usize = 8 << 20;
const MAX_CONNECTIONS: usize = 64;
const ACCEPT_BACKOFF: Duration = Duration::from_millis(100);

/// What every HTTP request is checked against and answered with.
pub struct Server {
    endpoint: String,
    plugins: Plugins,
    token: String,
}

impl Server {
    /// A server whose tools go to the daemon at `endpoint`; a request without `Authorization: Bearer <token>` gets 401.
    pub fn new(endpoint: String, plugins: Plugins, token: String) -> Self {
        Self {
            endpoint,
            plugins,
            token,
        }
    }

    /// Binds `addr` and serves until the process ends; an empty token is refused.
    pub async fn serve(self, addr: &str) -> Result<(), String> {
        if self.token.is_empty() {
            return Err(format!(
                "set {TOKEN_ENV}: every request must carry it as a bearer token"
            ));
        }
        let listener = TcpListener::bind(addr)
            .await
            .map_err(|e| format!("bind {addr}: {e}"))?;
        let local = listener
            .local_addr()
            .map_err(|e| format!("bind {addr}: {e}"))?;
        log(format!("MCP over HTTP at http://{local}{PATH}"));
        self.run(listener).await
    }

    async fn run(self, listener: TcpListener) -> Result<(), String> {
        let server = Arc::new(self);
        let slots = Arc::new(Semaphore::new(MAX_CONNECTIONS));
        loop {
            let slot = Arc::clone(&slots)
                .acquire_owned()
                .await
                .map_err(|e| format!("connection slots: {e}"))?;
            let stream = match listener.accept().await {
                Ok((stream, _)) => stream,
                Err(e) => {
                    log(format!("accept: {e}"));
                    tokio::time::sleep(ACCEPT_BACKOFF).await;
                    continue;
                }
            };
            let server = Arc::clone(&server);
            // detached: the connection owns its slot and ends with its client
            tokio::spawn(async move {
                let service =
                    service_fn(|req| async { Ok::<_, Infallible>(server.respond(req).await) });
                if let Err(e) = http1::Builder::new()
                    .serve_connection(TokioIo::new(stream), service)
                    .await
                {
                    log(format!("http: {e}"));
                }
                drop(slot);
            });
        }
    }

    async fn respond(&self, req: Request<Incoming>) -> Response<Full<Bytes>> {
        if req.uri().path() != PATH {
            return status(StatusCode::NOT_FOUND);
        }
        if !local_origin(req.headers().get(ORIGIN)) {
            return status(StatusCode::FORBIDDEN);
        }
        if !self.authorized(req.headers().get(AUTHORIZATION)) {
            let mut out = status(StatusCode::UNAUTHORIZED);
            out.headers_mut()
                .insert(WWW_AUTHENTICATE, HeaderValue::from_static("Bearer"));
            return out;
        }
        if req.method() != Method::POST {
            let mut out = status(StatusCode::METHOD_NOT_ALLOWED);
            out.headers_mut()
                .insert(ALLOW, HeaderValue::from_static("POST"));
            return out;
        }
        let body = match Limited::new(req.into_body(), MAX_BODY).collect().await {
            Ok(body) => body.to_bytes(),
            Err(e) if e.is::<LengthLimitError>() => return status(StatusCode::PAYLOAD_TOO_LARGE),
            Err(_) => return status(StatusCode::BAD_REQUEST),
        };
        match mcp::answer_one(&body, &self.endpoint, &self.plugins).await {
            None => status(StatusCode::ACCEPTED),
            Some(reply) => {
                let code = if reply["id"].is_null() {
                    StatusCode::BAD_REQUEST
                } else {
                    StatusCode::OK
                };
                json(code, &reply)
            }
        }
    }

    fn authorized(&self, header: Option<&HeaderValue>) -> bool {
        header
            .and_then(|h| h.as_bytes().strip_prefix(b"Bearer "))
            .is_some_and(|got| same(got, self.token.as_bytes()))
    }
}

/// Whether a browser `Origin` names this host; a request without one comes from a non-browser client.
fn local_origin(origin: Option<&HeaderValue>) -> bool {
    let Some(origin) = origin else {
        return true;
    };
    origin
        .to_str()
        .ok()
        .and_then(|o| o.parse::<Uri>().ok())
        .is_some_and(|u| matches!(u.host(), Some("localhost" | "127.0.0.1" | "[::1]")))
}

/// Compares in time that does not depend on where the inputs differ.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn status(code: StatusCode) -> Response<Full<Bytes>> {
    let mut out = Response::new(Full::new(Bytes::new()));
    *out.status_mut() = code;
    out
}

fn json(code: StatusCode, reply: &Value) -> Response<Full<Bytes>> {
    let mut out = Response::new(Full::new(Bytes::from(
        serde_json::to_vec(reply).unwrap_or_default(),
    )));
    *out.status_mut() = code;
    out.headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    out
}

#[cfg(test)]
mod tests {
    use std::net::SocketAddr;

    use serde_json::json;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpStream;

    use super::*;

    const TOKEN: &str = "s3cret";
    const BEARER: (&str, &str) = ("authorization", "Bearer s3cret");

    async fn start() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let server = Server::new("unused".to_owned(), Plugins::default(), TOKEN.to_owned());
        tokio::spawn(server.run(listener));
        addr
    }

    fn request(method: &str, path: &str, headers: &[(&str, &str)], body: &str) -> String {
        let extra: String = headers
            .iter()
            .map(|(k, v)| format!("{k}: {v}\r\n"))
            .collect();
        format!(
            "{method} {path} HTTP/1.1\r\nhost: localhost\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n{extra}\r\n{body}",
            body.len()
        )
    }

    async fn send(addr: SocketAddr, raw: &[u8]) -> (u16, String, String) {
        let mut conn = TcpStream::connect(addr).await.unwrap();
        conn.write_all(raw).await.unwrap();
        let mut out = Vec::new();
        conn.read_to_end(&mut out).await.unwrap();
        let out = String::from_utf8_lossy(&out).into_owned();
        let (head, body) = out.split_once("\r\n\r\n").unwrap_or((&out, ""));
        let code = head
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        (code, head.to_ascii_lowercase(), body.to_owned())
    }

    async fn call(
        addr: SocketAddr,
        method: &str,
        path: &str,
        headers: &[(&str, &str)],
        body: &str,
    ) -> (u16, String, String) {
        let headers = [&[BEARER], headers].concat();
        send(addr, request(method, path, &headers, body).as_bytes()).await
    }

    #[tokio::test]
    async fn a_request_gets_its_json_reply_and_a_notification_gets_202() {
        let addr = start().await;
        let (code, head, body) = call(addr, "POST", PATH, &[], &json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}).to_string()).await;
        assert_eq!(code, 200, "{head}");
        assert!(head.contains("content-type: application/json"), "{head}");
        let reply: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(reply["id"], 1);
        assert_eq!(reply["result"]["protocolVersion"], "2025-06-18");
        let (code, _, body) = call(
            addr,
            "POST",
            PATH,
            &[],
            &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}).to_string(),
        )
        .await;
        assert_eq!((code, body.as_str()), (202, ""));
        let (code, _, body) = call(addr, "POST", PATH, &[], &json!({"jsonrpc": "2.0", "id": "s", "method": "tools/call", "params": {"name": "status"}}).to_string()).await;
        assert_eq!(code, 200);
        assert_eq!(
            serde_json::from_str::<Value>(&body).unwrap()["result"]["isError"],
            false
        );
    }

    #[tokio::test]
    async fn path_method_origin_and_body_are_checked() {
        let addr = start().await;
        let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        assert_eq!(call(addr, "POST", "/other", &[], &ping).await.0, 404);
        let (code, head, _) = call(addr, "GET", PATH, &[], "").await;
        assert_eq!(code, 405);
        assert!(head.contains("allow: post"), "{head}");
        assert_eq!(
            call(
                addr,
                "POST",
                PATH,
                &[("origin", "https://evil.example")],
                &ping
            )
            .await
            .0,
            403
        );
        assert_eq!(
            call(
                addr,
                "POST",
                PATH,
                &[("origin", "http://localhost:3000")],
                &ping
            )
            .await
            .0,
            200
        );
        assert_eq!(call(addr, "POST", PATH, &[], "not json").await.0, 400);
        assert_eq!(
            call(addr, "POST", PATH, &[], &format!("[{ping}]")).await.0,
            400
        );
        assert_eq!(
            call(addr, "POST", PATH, &[], &" ".repeat(MAX_BODY + 1))
                .await
                .0,
            413
        );
    }

    #[tokio::test]
    async fn the_token_is_required_on_every_request() {
        let addr = start().await;
        let ping = json!({"jsonrpc": "2.0", "id": 1, "method": "ping"}).to_string();
        let unsent = |method: &str, headers: &[(&str, &str)], body: &str| {
            let raw = request(method, PATH, headers, body);
            async move { send(addr, raw.as_bytes()).await }
        };
        let (code, head, _) = unsent("POST", &[], &ping).await;
        assert_eq!(code, 401);
        assert!(head.contains("www-authenticate: bearer"), "{head}");
        let wrong = [("authorization", "Bearer wrong")];
        assert_eq!(unsent("POST", &wrong, &ping).await.0, 401);
        assert_eq!(unsent("GET", &[], "").await.0, 401);
        assert_eq!(call(addr, "POST", PATH, &[], &ping).await.0, 200);
    }

    #[tokio::test]
    async fn a_bind_without_a_token_is_refused_even_on_loopback() {
        let served = tokio::time::timeout(
            Duration::from_secs(1),
            Server::new("unused".to_owned(), Plugins::default(), String::new())
                .serve("127.0.0.1:0"),
        )
        .await;
        let err = served.expect("a bind without a token served").unwrap_err();
        assert!(err.contains(TOKEN_ENV), "{err}");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_dropped_client_closes_its_daemon_connection() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let daemon = tokio::net::UnixListener::bind(&endpoint).unwrap();
        let (got, got_call) = tokio::sync::oneshot::channel();
        let (shut, closed) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut conn, _) = daemon.accept().await.unwrap();
            let call: desk_core::wire::Call = desk_core::wire::read_frame(&mut conn).await.unwrap();
            let _ = got.send(call.tool);
            let mut rest = Vec::new();
            let _ = conn.read_to_end(&mut rest).await;
            let _ = shut.send(());
        });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(Server::new(endpoint, Plugins::default(), TOKEN.to_owned()).run(listener));
        let mut conn = TcpStream::connect(addr).await.unwrap();
        let body = json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "desk", "arguments": {"verb": "windows"}}}).to_string();
        conn.write_all(request("POST", PATH, &[BEARER], &body).as_bytes())
            .await
            .unwrap();
        assert_eq!(got_call.await.unwrap(), "desk_windows");
        drop(conn);
        tokio::time::timeout(Duration::from_secs(2), closed)
            .await
            .expect("the dropped client's daemon connection stayed open")
            .unwrap();
    }
}
