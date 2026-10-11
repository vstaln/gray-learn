//! gray-learn — the agent's memory of non-obvious lessons, on the folded-in
//! continual-harness store (prime-agent mechanics port — no prime code).
//!
//! The `learn` tool saves lessons as memory-kind harness entries
//! (`~/.gray/harness/{state.json,local/<cwd-hash>.json}`); the legacy
//! `~/.gray/learn/*.md` files import once and rename to `*.imported`.
//! `prompt/context` injects "## Learned lessons" (project, then global)
//! plus a bounded "## Harness" digest of the other kinds.
//!
//! Design: lessons are Reflexion — memory written from real signals, not
//! self-opinion; the store is files because compaction drops in-context
//! memory (Governance Decay); and *what* to keep is the model's call
//! through a tool (MemGPT).
//!
//! `/learn` lists/forgets/clears lessons and offers `distill`; `/harness`
//! drives the kinded store (list/show/delete/refine/rollback/status/on|off).
//! Auto-refine arms on `turn_end` (every 25 turns, 20min cooldown) and plans
//! through `host/run` when the `host.turn` capability is granted.

use std::io::{BufRead, Write};
use std::sync::mpsc;

use serde_json::{Value, json};

#[path = "mods/harness.rs"]
mod harness;

/// ~4KB cap on the injected lesson block (spec: "capped ~4KB").
const CONTEXT_CAP: usize = 4096;

fn manifest() -> Value {
    // The description is the memory policy: writes must tie to real signals,
    // and secrets never enter the store.
    let mut tools = vec![json!({
        "name": "learn",
        "description": "Save a non-obvious lesson so future sessions don't relearn it. \
            WHEN to save: verified build/test quirks, user preferences (stated or corrected), \
            pitfalls discovered while working, non-obvious codebase facts (hidden invariants, \
            surprising ownership). NEVER save: facts obvious from reading the code, secrets or \
            credentials, transient task state.",
        "parameters": {
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The lesson, one line, stated plainly."
                },
                "scope": {
                    "type": "string",
                    "enum": ["project", "global"],
                    "description": "'project' (default) applies only to this repo; \
                        'global' is injected in every project."
                }
            },
            "required": ["text"]
        }
    })];
    tools.extend(harness::manifest_tools());
    json!({
        "name": "learn",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.1",
        "tools": tools,
        "commands": ["/learn", "/harness"],
        "hooks": ["prompt/context"],
        "capabilities": ["host.turn", "host.say"],
        "subcommands": ["learn", "harness"],
    })
}

/// The `prompt/context` payload: "## Learned lessons" (project first, then
/// global — memory-kind entries), then the bounded "## Harness" digest of
/// the remaining kinds. `None` when nothing exists; project lessons lead.
fn context_text(cwd: &str) -> Option<String> {
    let project = harness::lessons("local", cwd);
    let global = harness::lessons("global", cwd);
    let digest = harness::digest(cwd);
    if project.is_empty() && global.is_empty() && digest.is_none() {
        return None;
    }
    let mut out = String::new();
    if !project.is_empty() || !global.is_empty() {
        out.push_str("## Learned lessons\n");
        for l in &project {
            out.push_str("- ");
            out.push_str(l);
            out.push('\n');
        }
        if !global.is_empty() {
            out.push_str("\nGlobal lessons:\n");
            for l in &global {
                out.push_str("- ");
                out.push_str(l);
                out.push('\n');
            }
        }
        if out.len() > CONTEXT_CAP {
            let mut cap = CONTEXT_CAP;
            while !out.is_char_boundary(cap) {
                cap -= 1;
            }
            let cut = out[..cap].rfind('\n').unwrap_or(cap);
            out.truncate(cut);
            out.push_str("\n- … (truncated)\n");
        }
        out.push_str("\n_You may save new non-obvious lessons with the `learn` tool._");
    }
    if let Some(d) = digest {
        if !out.is_empty() {
            out.push_str("\n\n");
        }
        out.push_str(&d);
    }
    Some(out)
}

/// `/learn …` — `argv` excludes the command name. `{"text"}` says it,
/// `{"prompt"}` runs it as a user prompt.
fn run_command(cwd: &str, argv: &[&str]) -> Value {
    match argv.first().copied() {
        None => {
            let project = harness::lessons("local", cwd);
            let global = harness::lessons("global", cwd);
            if project.is_empty() && global.is_empty() {
                return json!({ "text": format!(
                    "no lessons yet — the agent saves them with the `learn` tool\n\
                     lessons live in the harness store under ~/.gray/harness/ \
                     (project = local scope, global = ~/.gray/harness/state.json)"
                )});
            }
            let list = |lessons: &[String]| -> String {
                lessons
                    .iter()
                    .enumerate()
                    .take(40)
                    .map(|(i, l)| format!("  {}. {l}", i + 1))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            let mut text = String::new();
            if !project.is_empty() {
                text.push_str(&format!(
                    "project lessons ({}) — `forget <n>` removes:\n{}\n",
                    project.len(),
                    list(&project)
                ));
                if project.len() > 40 {
                    text.push_str(&format!("  … {} more\n", project.len() - 40));
                }
            }
            if !global.is_empty() {
                text.push_str(&format!(
                    "global lessons ({}):\n{}\n",
                    global.len(),
                    list(&global)
                ));
                if global.len() > 40 {
                    text.push_str(&format!("  … {} more\n", global.len() - 40));
                }
            }
            json!({ "text": text.trim_end() })
        }
        Some("forget") => {
            let n = argv.get(1).and_then(|s| s.parse::<usize>().ok());
            let res = match n {
                Some(n) => harness::learn_forget(cwd, n),
                None => Err("usage: /learn forget <n>".into()),
            };
            json!({ "text": res.unwrap_or_else(|e| e) })
        }
        Some("clear") => {
            let which = argv.get(1).copied().unwrap_or("project");
            json!({ "text": harness::learn_clear(cwd, which).unwrap_or_else(|e| e) })
        }
        Some("distill") => json!({ "prompt":
            "Review what you did and learned in this session. Save anything worth keeping \
             via the `learn` tool: verified build/test quirks, user preferences, pitfalls you \
             hit, non-obvious codebase facts. Use scope \"global\" only when a lesson applies \
             to every project. Skip facts obvious from the code and never save secrets."
        }),
        _ => json!({ "text": format!(
            "learn {} — the agent's memory on the harness store. /learn · /learn forget <n> · \
             /learn clear [project|global|all] · /learn distill · /harness list|show|delete|refine|rollback|status",
            env!("CARGO_PKG_VERSION")
        )}),
    }
}

/// cwd for the request: session.cwd, else params.cwd, else our cwd.
fn req_cwd(params: &Value) -> String {
    params["session"]["cwd"]
        .as_str()
        .or_else(|| params["cwd"].as_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| {
            std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default()
        })
}

/// One request → `Some(reply)`, or `None` for notifications. The bool asks
/// the loop to exit after writing the reply.
fn handle(sh: &harness::Shared, req: &Value) -> (Option<Value>, bool) {
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    if req.get("id").is_none() {
        match method {
            "plugin/shutdown" => return (None, true),
            "event/notify" => harness::on_event(sh, &req["params"]),
            _ => {}
        }
        return (None, method == "plugin/shutdown");
    }
    let id = req["id"].clone();
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let result = match method {
        "plugin/manifest" => manifest(),
        "tool/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("args").cloned().unwrap_or(Value::Null);
            if name == "learn" {
                let text = args.get("text").and_then(Value::as_str).unwrap_or("");
                match args.get("scope").and_then(Value::as_str) {
                    Some(s) if s != "project" && s != "global" => {
                        json!({ "content": format!("bad scope {s:?} — expected \"project\" or \"global\""),
                                "is_error": true })
                    }
                    other => {
                        let global = other == Some("global");
                        match harness::learn_save(&req_cwd(&params), text, global) {
                            Ok(t) => json!({ "content": t }),
                            Err(t) => json!({ "content": t, "is_error": true }),
                        }
                    }
                }
            } else {
                harness::tool_call(sh, &params)
            }
        }
        "command/run" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            if matches!(name, "/harness" | "harness") {
                json!({ "text": harness::command(sh, &params) })
            } else {
                let argv: Vec<&str> = params
                    .get("argv")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                run_command(&req_cwd(&params), &argv)
            }
        }
        "prompt/context" => match context_text(&req_cwd(&params)) {
            Some(text) => json!({ "text": text }),
            None => json!({}),
        },
        "plugin/shutdown" => return (Some(json!({ "id": id, "result": {} })), true),
        _ => {
            let error = json!({ "code": -32601, "message": "method not found" });
            return (Some(json!({ "id": id, "error": error })), false);
        }
    };
    (Some(json!({ "id": id, "result": result })), false)
}

fn sidecar() -> std::io::Result<()> {
    let sh = harness::Shared::new();
    let pending = sh.pending.clone();
    let (work_tx, work_rx) = mpsc::channel::<Value>();

    // Reader thread: host→sidecar replies carry a string id and no method —
    // route them to whoever is blocking on host/run; requests go to the work
    // loop. (The harness refinement planner needs this bridge.)
    let _reader = std::thread::spawn(move || {
        let stdin = std::io::stdin();
        for line in stdin.lock().lines() {
            let Ok(line) = line else { break };
            let Ok(v) = serde_json::from_str::<Value>(&line) else { continue };
            if v.get("method").is_none()
                && let Some(id) = v.get("id").and_then(Value::as_str)
                && let Some(tx) = pending.lock().expect("pending").remove(id)
            {
                let _ = tx.send(v);
                continue;
            }
            if work_tx.send(v).is_err() {
                break;
            }
        }
    });

    for req in work_rx {
        let (reply, exit) = handle(&sh, &req);
        if let Some(reply) = reply {
            let mut o = sh.out.lock().expect("stdout");
            let _ = writeln!(o, "{reply}");
            let _ = o.flush();
        }
        if exit {
            break;
        }
    }
    Ok(())
}

fn main() -> std::io::Result<()> {
    let argv: Vec<String> = std::env::args().skip(1).collect();
    match argv.first().map(String::as_str) {
        Some("manifest") => {
            println!("{}", manifest());
            Ok(())
        }
        Some("harness") => {
            println!("{}", harness::cli(&argv[1..].to_vec()));
            Ok(())
        }
        Some("learn") => {
            let cwd = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let args: Vec<&str> = argv[1..].iter().map(String::as_str).collect();
            let v = run_command(&cwd, &args);
            println!(
                "{}",
                v["text"].as_str().or_else(|| v["prompt"].as_str()).unwrap_or("")
            );
            Ok(())
        }
        Some("sidecar") => sidecar(),
        None => sidecar(),
        _ => sidecar(),
    }
}
