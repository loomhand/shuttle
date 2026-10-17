//! The MCP server on stdio: newline-delimited JSON-RPC 2.0 with the tools of `tools`.

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::{NAME, VERSION, tools};

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INSTRUCTIONS: &str = "These tools act on the user's own desktop. GUI work in a desktop app goes to gui_plan, the whole plan in one call; a loop or logic a plan cannot hold goes to one script_run; one look or one click goes to desk or screenshot. status says which tools this release runs.";

/// Answers MCP requests from `input` on `output`, one JSON line each way; desk tools go to the daemon at `endpoint`.
pub async fn serve<R, W>(input: R, mut output: W, endpoint: &str) -> Result<(), String>
where
    R: AsyncBufRead + Unpin,
    W: AsyncWrite + Unpin,
{
    let mut lines = input.lines();
    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|e| format!("read stdin: {e}"))?
    {
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = answer(&line, endpoint).await else {
            continue;
        };
        let mut body = serde_json::to_vec(&reply).map_err(|e| format!("encode reply: {e}"))?;
        body.push(b'\n');
        output
            .write_all(&body)
            .await
            .map_err(|e| format!("write stdout: {e}"))?;
        output
            .flush()
            .await
            .map_err(|e| format!("write stdout: {e}"))?;
    }
    Ok(())
}

async fn answer(line: &str, endpoint: &str) -> Option<Value> {
    let mut msg: Map<String, Value> = match serde_json::from_str(line) {
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
    let params = msg.remove("params").unwrap_or(Value::Null);
    let result = match method.as_str() {
        "initialize" => Ok(initialize(&params)),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(tools::list()),
        "tools/call" => {
            let mut params = params;
            let args = params
                .get_mut("arguments")
                .map_or_else(|| json!({}), Value::take);
            let name = params["name"].as_str().unwrap_or_default();
            tools::call(name, args, endpoint)
                .await
                .ok_or_else(|| (INVALID_PARAMS, format!("unknown tool {name}")))
        }
        _ => Err((METHOD_NOT_FOUND, format!("unknown method {method}"))),
    };
    Some(reply(id, result))
}

fn initialize(params: &Value) -> Value {
    let asked = params["protocolVersion"].as_str().unwrap_or_default();
    let version = PROTOCOL_VERSIONS
        .into_iter()
        .find(|v| *v == asked)
        .unwrap_or(PROTOCOL_VERSIONS[0]);
    json!({
        "protocolVersion": version,
        "capabilities": {"tools": {}},
        "serverInfo": {"name": NAME, "version": VERSION},
        "instructions": INSTRUCTIONS,
    })
}

fn reply(id: Value, result: Result<Value, (i64, String)>) -> Value {
    let mut out = json!({"jsonrpc": "2.0"});
    out["id"] = id;
    match result {
        Ok(r) => out["result"] = r,
        Err((code, message)) => out["error"] = json!({"code": code, "message": message}),
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
        exchange(
            &lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
            endpoint,
        )
        .await
    }

    async fn exchange(input: &str, endpoint: &str) -> Vec<Value> {
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output, endpoint)
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
            async move { server::serve(Arc::new(Fake), &endpoint).await }
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
}
