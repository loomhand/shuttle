//! The MCP tools: their schemas, and the answers of this release, which lists them before it runs them.

use serde_json::{Value, json};

use crate::{NAME, VERSION};

const NOT_YET: &str = "this release lists the tool but does not run it yet; see docs/platforms.md for when it lands on this OS";
const DESK_VERBS: [&str; 15] = [
    "windows", "snapshot", "find", "read", "press", "act", "at", "key", "type", "click", "focus",
    "close", "launch", "shot", "screen",
];

/// The tool list of `tools/list`.
pub fn list() -> Value {
    json!([
        {
            "name": "gui_plan",
            "description": "Run a desktop-app GUI plan in one call: each step runs on the accessibility tree, is checked, and the plan stops at the first surprise. One op per step: {app}, {menu}, {click}, {check, on}, {set, value}, {type}, {key}, {wait}, {verify}, {close: true}. A passing plan can be saved as a flow with save, and replayed with flow and params.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "steps": {"type": "array", "items": {"type": "object"}, "description": "The plan, one op per step"},
                    "flow": {"type": "string", "description": "Replay this saved flow instead of steps"},
                    "params": {"type": "object", "description": "Values for the {{name}} slots"},
                    "save": {"type": "string", "description": "Flow name for this plan"},
                    "start": {"type": "integer", "description": "First step to run, default 0"},
                    "find": {"type": "string", "description": "A request: returns the saved flow that does it"},
                    "timeout_s": {"type": "integer", "description": "Default 90, max 110"}
                }
            }
        },
        {
            "name": "script_run",
            "description": "Run one JavaScript or TypeScript ES module on the bundled Node runtime with the desk API: windows, accessibility snapshots, find and press elements, keys, typing, clicks, screen crops and model calls. A module that exports main(params) is called with params and its return value comes back as result.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "code": {"type": "string", "description": "The script as an ES module"},
                    "script": {"type": "string", "description": "Path of a script file"},
                    "params": {"type": "object", "description": "Passed to the script's exported main(params)"},
                    "timeout_s": {"type": "integer", "description": "Default 90, max 110"}
                }
            }
        },
        {
            "name": "desk",
            "description": "One desk call on this desktop: list windows, snapshot or search a window's accessibility tree, read, press or act on an element, send keys, type, click, focus, close or launch, or crop the screen.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "verb": {"type": "string", "enum": DESK_VERBS},
                    "args": {"type": "object", "description": "The verb's arguments, as the desk wire takes them"}
                },
                "required": ["verb"]
            }
        },
        {
            "name": "screenshot",
            "description": "A PNG of the screen, or of the rectangle x, y, w, h in screen pixels.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "x": {"type": "integer"},
                    "y": {"type": "integer"},
                    "w": {"type": "integer"},
                    "h": {"type": "integer"}
                }
            }
        },
        {
            "name": "status",
            "description": "This host's version, OS and architecture, and which tools this release runs.",
            "inputSchema": {"type": "object", "properties": {}}
        }
    ])
}

/// Answers a `tools/call` of `name`: an MCP tool result, or `None` for an unknown tool.
pub fn call(name: &str) -> Option<Value> {
    match name {
        "status" => Some(result(&status().to_string(), false)),
        "gui_plan" | "script_run" | "desk" | "screenshot" => Some(result(NOT_YET, true)),
        _ => None,
    }
}

fn status() -> Value {
    json!({
        "name": NAME,
        "version": VERSION,
        "os": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "runs": ["status"],
    })
}

fn result(text: &str, is_error: bool) -> Value {
    json!({"content": [{"type": "text", "text": text}], "isError": is_error})
}
