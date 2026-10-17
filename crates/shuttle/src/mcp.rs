//! The MCP server on stdio: newline-delimited JSON-RPC 2.0 with the tools of `tools`.

use std::io::{BufRead, Write};

use serde_json::{Map, Value, json};

use crate::{NAME, VERSION, tools};

const PROTOCOL_VERSIONS: [&str; 3] = ["2025-11-25", "2025-06-18", "2025-03-26"];
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INSTRUCTIONS: &str = "These tools act on the user's own desktop. GUI work in a desktop app goes to gui_plan, the whole plan in one call; a loop or logic a plan cannot hold goes to one script_run; one look or one click goes to desk or screenshot. status says which tools this release runs.";

/// Answers the MCP requests read from `input` until it ends, one JSON line each way on `output`.
pub fn serve(input: impl BufRead, mut output: impl Write) -> Result<(), String> {
    for line in input.lines() {
        let line = line.map_err(|e| format!("read stdin: {e}"))?;
        if line.trim().is_empty() {
            continue;
        }
        let Some(reply) = answer(&line) else {
            continue;
        };
        let mut body = serde_json::to_vec(&reply).map_err(|e| format!("encode reply: {e}"))?;
        body.push(b'\n');
        output
            .write_all(&body)
            .and_then(|()| output.flush())
            .map_err(|e| format!("write stdout: {e}"))?;
    }
    Ok(())
}

fn answer(line: &str) -> Option<Value> {
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
            let name = params["name"].as_str().unwrap_or_default();
            tools::call(name).ok_or_else(|| (INVALID_PARAMS, format!("unknown tool {name}")))
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
        Err((code, message)) => {
            out["error"] = json!({"code": code});
            out["error"]["message"] = message.into();
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn session(lines: &[Value]) -> Vec<Value> {
        exchange(&lines.iter().map(|l| format!("{l}\n")).collect::<String>())
    }

    fn exchange(input: &str) -> Vec<Value> {
        let mut output = Vec::new();
        serve(input.as_bytes(), &mut output).unwrap();
        output
            .split(|b| *b == b'\n')
            .filter(|l| !l.is_empty())
            .map(|l| serde_json::from_slice(l).unwrap())
            .collect()
    }

    #[test]
    fn initialize_lists_five_tools_and_status_runs() {
        let out = session(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "initialize", "params": {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "t", "version": "0"}}}),
            json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
            json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
            json!({"jsonrpc": "2.0", "id": 3, "method": "tools/call", "params": {"name": "status", "arguments": {}}}),
        ]);
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

    #[test]
    fn unlisted_protocol_version_gets_the_newest() {
        let out = session(&[
            json!({"jsonrpc": "2.0", "id": "a", "method": "initialize", "params": {"protocolVersion": "2024-11-05"}}),
        ]);
        assert_eq!(out[0]["id"], "a");
        assert_eq!(out[0]["result"]["protocolVersion"], PROTOCOL_VERSIONS[0]);
    }

    #[test]
    fn tools_not_run_yet_answer_a_tool_error() {
        let out = session(&[
            json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call", "params": {"name": "gui_plan", "arguments": {"steps": []}}}),
        ]);
        assert_eq!(out[0]["result"]["isError"], true);
        let text = out[0]["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("does not run it yet"), "{text}");
    }

    #[test]
    fn bad_requests_get_json_rpc_errors() {
        let input = "not json\n{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"nope\"}\n{\"jsonrpc\":\"2.0\",\"id\":2,\"method\":\"tools/call\",\"params\":{\"name\":\"rm\"}}\n{\"jsonrpc\":\"2.0\",\"id\":3}\n";
        let out = exchange(input);
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
}
