//! The MCP server on stdio: newline-delimited JSON-RPC 2.0 with the tools of `tools` and the resources of the script plugins.

use std::collections::VecDeque;
use std::pin::Pin;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::plugins::Plugins;
use crate::{NAME, VERSION, tools};

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INSTRUCTIONS: &str = "These tools act on the user's own desktop. GUI work in a desktop app goes to gui_plan, the whole plan in one call; a loop or logic a plan cannot hold goes to one script_run; one look or one click goes to desk or screenshot.";

type Answer<'a> = Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>>;

/// Answers MCP requests from `input` on `output` in order, one JSON line each way; desk and plugin tools go to the daemon at `endpoint`.
/// A `notifications/cancelled` drops the request it names without a reply, and a running one closes its daemon connection.
pub async fn serve<R, W>(
    input: R,
    mut output: W,
    endpoint: &str,
    plugins: &Plugins,
) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = input.lines();
    let mut queued: VecDeque<String> = VecDeque::new();
    let mut running: Option<(Option<String>, Answer<'_>)> = None;
    let mut open = true;
    while open || running.is_some() || !queued.is_empty() {
        if running.is_none()
            && let Some(line) = queued.pop_front()
        {
            running = Some((request_id(&line), Box::pin(answer(line, endpoint, plugins))));
        }
        tokio::select! {
            line = lines.next_line(), if open => match line.map_err(|e| format!("read stdin: {e}"))? {
                None => open = false,
                Some(line) if line.trim().is_empty() => {}
                Some(line) => match cancelled(&line) {
                    Some(id) if running.as_ref().is_some_and(|(r, _)| r.as_ref() == Some(&id)) => running = None,
                    Some(id) => queued.retain(|q| request_id(q).as_ref() != Some(&id)),
                    None => queued.push_back(line),
                },
            },
            reply = async {
                match running.as_mut() {
                    Some((_, answering)) => answering.await,
                    None => std::future::pending().await,
                }
            }, if running.is_some() => {
                running = None;
                if let Some(reply) = reply {
                    write(&mut output, &reply).await?;
                }
            }
        }
    }
    Ok(())
}

async fn write<W: AsyncWrite + Unpin>(output: &mut W, reply: &Value) -> Result<(), String> {
    output
        .write_all(format!("{reply}\n").as_bytes())
        .await
        .map_err(|e| format!("write stdout: {e}"))?;
    output
        .flush()
        .await
        .map_err(|e| format!("write stdout: {e}"))
}

/// The id of the request on `line` as JSON text; none for a notification or a line that does not parse.
fn request_id(line: &str) -> Option<String> {
    let msg: Value = serde_json::from_str(line).ok()?;
    msg.get("id").map(Value::to_string)
}

/// The request id a `notifications/cancelled` on `line` names, as JSON text.
fn cancelled(line: &str) -> Option<String> {
    let msg: Value = serde_json::from_str(line).ok()?;
    if msg["method"] != "notifications/cancelled" || msg.get("id").is_some() {
        return None;
    }
    msg["params"].get("requestId").map(Value::to_string)
}

async fn answer(line: String, endpoint: &str, plugins: &Plugins) -> Option<Value> {
    let mut msg: Map<String, Value> = match serde_json::from_str(&line) {
        Ok(m) => m,
        Err(e) => {
            return Some(reply(
                Value::Null,
                Err((PARSE_ERROR, format!("parse: {e}"))),
            ));
        }
    };
    let id = msg.remove("id")?;
    let Some(Value::String(method)) = msg.remove("method") else {
        return Some(reply(id, Err((INVALID_REQUEST, "no method".to_owned()))));
    };
    let mut params = msg.remove("params").unwrap_or(Value::Null);
    let result = match method.as_str() {
        "initialize" => Ok(initialize(&params, plugins)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list(plugins)),
        "resources/list" => Ok(resources(plugins)),
        "resources/read" => read(&params, plugins).await,
        "tools/call" => {
            let args = params
                .get_mut("arguments")
                .map_or_else(|| json!({}), Value::take);
            let name = params["name"].as_str().unwrap_or_default();
            tools::call(name, args, endpoint, plugins)
                .await
                .ok_or_else(|| (INVALID_PARAMS, format!("unknown tool {name}")))
        }
        _ => Err((METHOD_NOT_FOUND, format!("unknown method {method}"))),
    };
    Some(reply(id, result))
}

fn initialize(params: &Value, plugins: &Plugins) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .into_iter()
        .find(|v| *v == asked)
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    let mut out = json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": NAME, "version": VERSION},
        "instructions": INSTRUCTIONS,
    });
    if plugins.runs.iter().any(|p| !p.resources.is_empty()) {
        out["capabilities"]["resources"] = json!({});
    }
    out
}

fn resources(plugins: &Plugins) -> Value {
    let listed: Vec<Value> = plugins
        .runs
        .iter()
        .flat_map(|p| &p.resources)
        .map(|r| {
            let name = r.path.file_name().map(|n| n.to_string_lossy());
            json!({"uri": r.uri, "name": name, "mimeType": r.mime, "description": r.description})
        })
        .collect();
    json!({"resources": listed})
}

async fn read(params: &Value, plugins: &Plugins) -> Result<Value, (i64, String)> {
    let uri = params["uri"].as_str().unwrap_or_default();
    let r = plugins
        .runs
        .iter()
        .flat_map(|p| &p.resources)
        .find(|r| r.uri == uri)
        .ok_or_else(|| (INVALID_PARAMS, format!("unknown resource {uri}")))?;
    let text = tokio::fs::read_to_string(&r.path)
        .await
        .map_err(|e| (INVALID_PARAMS, format!("read {}: {e}", r.path.display())))?;
    Ok(json!({"contents": [{"uri": r.uri, "mimeType": r.mime, "text": text}]}))
}

fn reply(id: Value, result: Result<Value, (i64, String)>) -> Value {
    let mut out = json!({"jsonrpc": "2.0"});
    out["id"] = id;
    match result {
        Ok(r) => out["result"] = r,
        Err((code, message)) => {
            out["error"] = json!({"code": code});
            out["error"]["message"] = message.into();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::sync::Arc;

    #[cfg(unix)]
    use desk_core::wire::{Call, Output};
    #[cfg(unix)]
    use shuttle_desk::server::{self, Handler};

    use super::*;

    async fn session(lines: &[Value], endpoint: &str) -> Vec<Value> {
        session_with(lines, endpoint, &Plugins::default()).await
    }

    async fn session_with(lines: &[Value], endpoint: &str, plugins: &Plugins) -> Vec<Value> {
        exchange_with(
            &lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
            endpoint,
            plugins,
        )
        .await
    }

    async fn exchange(input: &str, endpoint: &str) -> Vec<Value> {
        exchange_with(input, endpoint, &Plugins::default()).await
    }

    async fn exchange_with(input: &str, endpoint: &str, plugins: &Plugins) -> Vec<Value> {
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, endpoint, plugins)
            .await
            .unwrap();
        output
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect()
    }

    #[tokio::test]
    async fn initialize_lists_five_tools_and_status_runs() {
        let out = session(
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
                json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
                json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
            ],
            "unused",
        )
        .await;
        assert_eq!(out.len(), 3, "the notification gets no reply: {out:?}");
        assert_eq!(out[0]["result"]["protocolVersion"], "2025-06-18");
        assert_eq!(out[0]["result"]["serverInfo"]["name"], NAME);
        assert!(INSTRUCTIONS.len() <= 2048);
        let names: Vec<&str> = out[1]["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            ["gui_plan", "script_run", "desk", "screenshot", "status"]
        );
        assert_eq!(out[2]["id"], 3);
        assert_eq!(out[2]["result"]["isError"], false);
        let status: Value =
            serde_json::from_str(out[2]["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(status["version"], VERSION);
    }

    #[tokio::test]
    async fn unlisted_protocol_version_gets_the_newest() {
        let out = session(
            &[json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {"protocolVersion": "2024-11-05"}})],
            "unused",
        )
        .await;
        assert_eq!(out[0]["id"], "a");
        assert_eq!(out[0]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[tokio::test]
    async fn bad_requests_get_json_rpc_errors() {
        let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"nope\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"rm\"}}\n{\"jsonrpc\":\"2.0\",\"id\":3}\n";
        let out = exchange(input, "unused").await;
        let codes: Vec<i64> = out
            .iter()
            .map(|r| r["error"]["code"].as_i64().unwrap())
            .collect();
        assert_eq!(
            codes,
            [
                PARSE_ERROR,
                METHOD_NOT_FOUND,
                INVALID_PARAMS,
                INVALID_REQUEST
            ]
        );
        assert_eq!(out[0]["id"], Value::Null);
        assert_eq!(out[2]["error"]["message"], "unknown tool rm");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cancelled_call_closes_its_daemon_connection_gets_no_reply_and_later_requests_go_on()
    {
        use tokio::io::{AsyncReadExt, BufReader};

        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let daemon = tokio::net::UnixListener::bind(&endpoint).unwrap();
        let (got, got_call) = tokio::sync::oneshot::channel();
        let (shut, closed) = tokio::sync::oneshot::channel();
        tokio::spawn(async move {
            let (mut conn, _) = daemon.accept().await.unwrap();
            let call: Call = desk_core::wire::read_frame(&mut conn).await.unwrap();
            let _ = got.send(call.tool);
            let mut rest = Vec::new();
            let _ = conn.read_to_end(&mut rest).await;
            let _ = shut.send(());
        });
        let (mut to_mcp, input) = tokio::io::duplex(1 << 16);
        let (output, from_mcp) = tokio::io::duplex(1 << 16);
        let mcp = tokio::spawn({
            let endpoint = endpoint.clone();
            async move {
                serve(
                    BufReader::new(input),
                    output,
                    &endpoint,
                    &Plugins::default(),
                )
                .await
            }
        });
        let line = |v: Value| format!("{v}\n").into_bytes();
        to_mcp
            .write_all(&line(json!({"jsonrpc": "2.0", "id": 7, "method": "tools/call", "params": {"name": "desk", "arguments": {"verb": "windows"}}})))
            .await
            .unwrap();
        assert_eq!(got_call.await.unwrap(), "desk_windows");
        to_mcp
            .write_all(&line(json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 7}})))
            .await
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(2), closed)
            .await
            .expect("the cancelled call's daemon connection stayed open")
            .unwrap();
        to_mcp
            .write_all(&line(json!({"jsonrpc": "2.0", "id": 8, "method": "ping"})))
            .await
            .unwrap();
        drop(to_mcp);
        mcp.await.unwrap().unwrap();
        let mut replies = BufReader::new(from_mcp).lines();
        let mut ids = Vec::new();
        while let Some(l) = replies.next_line().await.unwrap() {
            ids.push(serde_json::from_str::<Value>(&l).unwrap()["id"].clone());
        }
        assert_eq!(ids, [json!(8)]);
    }

    #[cfg(unix)]
    struct Slow;

    #[cfg(unix)]
    impl Handler for Slow {
        async fn answer(&self, _: Call) -> Output {
            tokio::time::sleep(std::time::Duration::from_millis(300)).await;
            Output::text("[]")
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_cancel_drops_a_queued_request_and_replies_keep_their_order() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let daemon = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { server::serve(Arc::new(Slow), server::bind(&endpoint).await?).await }
        });
        for _ in 0..50 {
            if server::connect(&endpoint).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let windows = |id: i64| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "desk", "arguments": {"verb": "windows"}}});
        let out = session(
            &[
                windows(1),
                windows(2),
                json!({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 2}}),
                json!({"jsonrpc": "2.0", "id": "3", "method": "ping"}),
            ],
            &endpoint,
        )
        .await;
        daemon.abort();
        let ids: Vec<&Value> = out.iter().map(|r| &r["id"]).collect();
        assert_eq!(ids, [&json!(1), &json!("3")], "{out:?}");
    }

    #[cfg(unix)]
    struct Fake;

    #[cfg(unix)]
    impl Handler for Fake {
        async fn answer(&self, call: Call) -> Output {
            match call.tool.as_str() {
                "desk_shot" => Output {
                    text: r#"{"ok":true,"width":2,"height":1}"#.into(),
                    image_b64: Some("UE5H".into()),
                    ..Output::default()
                },
                "desk_windows" => Output::text("[]"),
                "demo_run" => Output::text(format!("demo ran {}", call.args["code"])),
                other => Output::error(format!("{other} failed")),
            }
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn desk_and_screenshot_go_to_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let daemon = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { server::serve(Arc::new(Fake), server::bind(&endpoint).await?).await }
        });
        for _ in 0..50 {
            if server::connect(&endpoint).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let out = session(
            &[
                json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "desk", "arguments": {"verb": "windows"}}}),
                json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": "screenshot"}}),
                json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "desk", "arguments": {"verb": "press", "args": {"id": "1"}}}}),
                json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call", "params": {"name": "desk", "arguments": {"verb": "format"}}}),
            ],
            &endpoint,
        )
        .await;
        daemon.abort();
        assert_eq!(
            out[0]["result"],
            json!({"content": [{"type": "text", "text": "[]"}], "isError": false})
        );
        assert_eq!(
            out[1]["result"]["content"],
            json!([{"type": "text", "text": "{\"ok\":true,\"width\":2,\"height\":1}"}, {"type": "image", "mimeType": "image/png", "data": "UE5H"}])
        );
        assert_eq!(
            out[2]["result"],
            json!({"content": [{"type": "text", "text": "desk_press failed"}], "isError": true})
        );
        assert_eq!(out[3]["result"]["isError"], true);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_plugin_is_listed_runs_through_the_daemon_and_serves_its_resource() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let guide = dir.path().join("GUIDE.md");
        std::fs::write(&guide, "# Demo guide").unwrap();
        let plugin: crate::plugins::Plugin = serde_json::from_value(json!({
            "name": "demo_run",
            "description": "Run a demo script.",
            "bin": "/usr/bin/demo",
            "resources": [{"uri": "demo://GUIDE.md", "path": guide, "mime": "text/markdown", "description": "The demo guide"}]
        }))
        .unwrap();
        let daemon = tokio::spawn({
            let endpoint = endpoint.clone();
            async move { server::serve(Arc::new(Fake), server::bind(&endpoint).await?).await }
        });
        for _ in 0..50 {
            if server::connect(&endpoint).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        let lines = [
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18"}}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "demo_run", "arguments": {"code": "x"}}}),
            json!({"jsonrpc": "2.0", "id": 4, "method": "resources/list"}),
            json!({"jsonrpc": "2.0", "id": 5, "method": "resources/read", "params": {"uri": "demo://GUIDE.md"}}),
            json!({"jsonrpc": "2.0", "id": 6, "method": "resources/read", "params": {"uri": "demo://other"}}),
        ];
        let plugins = Plugins {
            runs: vec![plugin],
            services: Vec::new(),
        };
        let out = session_with(&lines, &endpoint, &plugins).await;
        let without = session(&lines[..3], &endpoint).await;
        daemon.abort();
        assert_eq!(out[0]["result"]["capabilities"]["resources"], json!({}));
        assert_eq!(out[1]["result"]["tools"][5]["name"], "demo_run");
        assert_eq!(out[2]["result"]["content"][0]["text"], "demo ran \"x\"");
        assert_eq!(
            out[3]["result"]["resources"],
            json!([{"uri": "demo://GUIDE.md", "name": "GUIDE.md", "mimeType": "text/markdown", "description": "The demo guide"}])
        );
        assert_eq!(
            out[4]["result"]["contents"],
            json!([{"uri": "demo://GUIDE.md", "mimeType": "text/markdown", "text": "# Demo guide"}])
        );
        assert_eq!(out[5]["error"]["code"], INVALID_PARAMS);
        assert!(
            without[0]["result"]["capabilities"]
                .get("resources")
                .is_none()
        );
        assert_eq!(without[1]["result"]["tools"].as_array().unwrap().len(), 5);
        assert_eq!(without[2]["error"]["message"], "unknown tool demo_run");
    }
}
