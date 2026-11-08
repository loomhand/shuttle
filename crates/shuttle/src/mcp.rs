//! The MCP server: newline-delimited JSON-RPC 2.0 on stdio with the tools of `tools` and the resources of the script plugins; `http` answers the same messages over HTTP.

use std::collections::VecDeque;
use std::hash::{BuildHasher, RandomState};
use std::pin::Pin;
use std::sync::Arc;

use serde_json::{Map, Value, json};
use tokio::io::{AsyncBufRead, AsyncBufReadExt, AsyncWrite, AsyncWriteExt};

use crate::plugins::Plugins;
use crate::{NAME, VERSION, tools};

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const AGENT_PART: usize = 32;
const INSTRUCTIONS: &str = "These tools act on the user's own desktop. GUI work in a desktop app goes to gui_plan, the whole plan in one call; a loop or logic a plan cannot hold goes to one script_run; one look or one click goes to desk or screenshot. To learn a task by watching the user, record_start, let them do it, then record_stop and run its draft with gui_plan.";

pub(crate) type Message = Result<Map<String, Value>, String>;
type Answer<'a> = Pin<Box<dyn Future<Output = Option<Value>> + Send + 'a>>;

/// Answers the MCP requests on `input` in order, one JSON line each way.
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
    let nonce = format!("{:08x}", RandomState::new().hash_one(()) as u32);
    let mut agent: Arc<str> = agent_id(&[&nonce]).into();
    let mut queued: VecDeque<Message> = VecDeque::new();
    let mut running: Option<(Option<String>, Answer<'_>)> = None;
    let mut open = true;
    while open || running.is_some() || !queued.is_empty() {
        if running.is_none()
            && let Some(msg) = queued.pop_front()
        {
            if method(&msg) == Some("initialize") {
                agent = agent_id(&[client(&msg), &nonce]).into();
            }
            let agent = Arc::clone(&agent);
            let id = request_id(&msg);
            running = Some((
                id,
                Box::pin(async move { answer(msg, &agent, endpoint, plugins).await }),
            ));
        }
        tokio::select! {
            line = lines.next_line(), if open => match line.map_err(|e| format!("read stdin: {e}"))? {
                None => open = false,
                Some(line) if line.trim().is_empty() => {}
                Some(line) => {
                    let msg = parse(line.as_bytes());
                    match cancelled(&msg) {
                        Some(id) if running.as_ref().is_some_and(|(r, _)| r.as_ref() == Some(&id)) => running = None,
                        Some(id) => queued.retain(|q| request_id(q).as_ref() != Some(&id)),
                        None => queued.push_back(msg),
                    }
                }
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

/// The `agent_id` the calls of one MCP session carry: `mcp`, then each non-empty part in letters, digits, `_` and `-`.
pub(crate) fn agent_id(parts: &[&str]) -> String {
    let mut id = "mcp".to_owned();
    for part in parts.iter().filter(|p| !p.is_empty()) {
        id.push('-');
        id.extend(part.chars().take(AGENT_PART).map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '-'
            }
        }));
    }
    id
}

pub(crate) fn parse(body: &[u8]) -> Message {
    serde_json::from_slice(body).map_err(|e| format!("parse: {e}"))
}

pub(crate) fn method(msg: &Message) -> Option<&str> {
    msg.as_ref().ok()?.get("method")?.as_str()
}

/// The id of the request `msg` as JSON text; none for a notification or a line that does not parse.
pub(crate) fn request_id(msg: &Message) -> Option<String> {
    msg.as_ref().ok()?.get("id").map(Value::to_string)
}

/// The request id a `notifications/cancelled` names, as JSON text.
pub(crate) fn cancelled(msg: &Message) -> Option<String> {
    let msg = msg.as_ref().ok()?;
    if msg.get("method").and_then(Value::as_str) != Some("notifications/cancelled")
        || msg.contains_key("id")
    {
        return None;
    }
    msg.get("params")?.get("requestId").map(Value::to_string)
}

/// Answers `msg` of the MCP session whose calls carry `agent`; none for a notification.
pub(crate) async fn answer(
    msg: Message,
    agent: &str,
    endpoint: &str,
    plugins: &Plugins,
) -> Option<Value> {
    let mut msg = match msg {
        Ok(m) => m,
        Err(e) => return Some(reply(Value::Null, Err((PARSE_ERROR, e)))),
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
            tools::call(name, args, agent, endpoint, plugins)
                .await
                .ok_or_else(|| (INVALID_PARAMS, format!("unknown tool {name}")))
        }
        _ => Err((METHOD_NOT_FOUND, format!("unknown method {method}"))),
    };
    Some(reply(id, result))
}

fn client(msg: &Message) -> &str {
    msg.as_ref()
        .ok()
        .and_then(|m| m.get("params")?.get("clientInfo")?.get("name")?.as_str())
        .unwrap_or_default()
}

async fn write<W: AsyncWrite + Unpin>(output: &mut W, reply: &Value) -> Result<(), String> {
    let sent = async {
        output.write_all(format!("{reply}\n").as_bytes()).await?;
        output.flush().await
    };
    sent.await.map_err(|e| format!("write stdout: {e}"))
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
    let mut out = json!({"contents": [{"uri": r.uri, "mimeType": r.mime}]});
    out["contents"][0]["text"] = text.into();
    Ok(out)
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
    use desk_core::wire::{Call, Output};
    #[cfg(unix)]
    use shuttle_desk::server::{self, Handler};

    use super::*;

    #[tokio::test]
    async fn initialize_lists_the_built_in_tools_and_status_runs() {
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
        assert_eq!(names, crate::tools::NAMES);
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
        let out = exchange(input, "unused", &Plugins::default()).await;
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
        use tokio::io::BufReader;

        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let (got_call, closed) = crate::one_call_daemon(&endpoint);
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
    #[tokio::test]
    async fn a_cancel_drops_a_queued_request_and_replies_keep_their_order() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let daemon = daemon(Slow, &endpoint).await;
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
    #[tokio::test]
    async fn desk_and_screenshot_go_to_the_daemon() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let daemon = daemon(Fake, &endpoint).await;
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
        let daemon = daemon(Fake, &endpoint).await;
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
        assert_eq!(
            out[1]["result"]["tools"][crate::tools::NAMES.len()]["name"],
            "demo_run"
        );
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
        assert_eq!(
            without[1]["result"]["tools"].as_array().unwrap().len(),
            crate::tools::NAMES.len()
        );
        assert_eq!(without[2]["error"]["message"], "unknown tool demo_run");
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn each_mcp_session_calls_the_daemon_as_its_own_agent() {
        let dir = tempfile::tempdir().unwrap();
        let endpoint = dir.path().join("desk.sock").to_string_lossy().into_owned();
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let daemon = daemon(Callers(Arc::clone(&seen)), &endpoint).await;
        let open = json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"clientInfo": {"name": "claude-code", "version": "2"}}});
        let web = |id: i64, on: bool| json!({"jsonrpc": "2.0", "id": id, "method": "tools/call", "params": {"name": "desk", "arguments": {"verb": "web", "args": {"pid": 7, "on": on}}}});
        session(&[open.clone(), web(2, true)], &endpoint).await;
        session(&[open, web(2, true), web(3, false)], &endpoint).await;
        daemon.abort();
        let seen = seen.lock().unwrap().clone();
        let agents: Vec<&str> = seen.iter().map(|(a, _)| a.as_str()).collect();
        assert_eq!(agents.len(), 3, "{seen:?}");
        assert!(
            agents.iter().all(|a| a.starts_with("mcp-claude-code-")),
            "{agents:?}"
        );
        assert_ne!(
            agents[0], agents[1],
            "two sessions of one client share an agent"
        );
        assert_eq!(agents[1], agents[2]);
        assert_eq!(seen[2].1, json!({"pid": 7, "on": false}));
    }

    #[test]
    fn an_agent_id_keeps_letters_digits_and_cuts_each_part() {
        assert_eq!(agent_id(&[]), "mcp");
        assert_eq!(agent_id(&["", "ab12"]), "mcp-ab12");
        assert_eq!(
            agent_id(&["My Client/1.0", "ab12"]),
            "mcp-My-Client-1-0-ab12"
        );
        assert_eq!(agent_id(&["run-3", "x"]), "mcp-run-3-x");
        assert_eq!(
            agent_id(&[&"é".repeat(40)]),
            format!("mcp-{}", "-".repeat(AGENT_PART))
        );
    }

    async fn session(lines: &[Value], endpoint: &str) -> Vec<Value> {
        session_with(lines, endpoint, &Plugins::default()).await
    }

    async fn session_with(lines: &[Value], endpoint: &str, plugins: &Plugins) -> Vec<Value> {
        exchange(
            &lines.iter().map(|l| format!("{l}\n")).collect::<String>(),
            endpoint,
            plugins,
        )
        .await
    }

    async fn exchange(input: &str, endpoint: &str, plugins: &Plugins) -> Vec<Value> {
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

    #[cfg(unix)]
    async fn daemon<H: Handler>(
        handler: H,
        endpoint: &str,
    ) -> tokio::task::JoinHandle<Result<(), String>> {
        let daemon = tokio::spawn({
            let endpoint = endpoint.to_owned();
            async move { server::serve(Arc::new(handler), server::bind(&endpoint).await?).await }
        });
        for _ in 0..50 {
            if server::connect(endpoint).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        daemon
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
    struct Callers(Arc<std::sync::Mutex<Vec<(String, Value)>>>);

    #[cfg(unix)]
    impl Handler for Callers {
        async fn answer(&self, call: Call) -> Output {
            self.0.lock().unwrap().push((call.agent_id, call.args));
            Output::text(r#"{"ok":true,"was":false}"#)
        }
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
}
