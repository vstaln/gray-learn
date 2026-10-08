//! gray-learn — the agent's memory of non-obvious lessons.
//!
//! The model saves lessons with the `learn` tool (verified build quirks,
//! user preferences, pitfalls, non-obvious codebase facts). They land in
//! `<gray-home>/learn/global.md` or
//! `<gray-home>/learn/projects/<sha1(cwd)[:12]>.md` and are injected into
//! future sessions via `prompt/context` under "## Learned lessons".
//!
//! `/learn` lists both files; `/learn forget <n>` drops a project lesson;
//! `/learn clear` wipes them; `/learn distill` asks the model to review the
//! session and save what's worth keeping.

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};

/// ~4KB cap on the injected lesson block (spec: "capped ~4KB").
const CONTEXT_CAP: usize = 4096;

fn manifest() -> Value {
    json!({
        "name": "learn",
        "version": env!("CARGO_PKG_VERSION"),
        "protocol": "1.1",
        "tools": [{
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
        }],
        "commands": ["/learn"],
        "hooks": ["prompt/context"],
    })
}

/// `<gray-home>` — honors `$GRAY_HOME`, falls back to `$HOME/.gray`.
fn gray_home() -> PathBuf {
    std::env::var_os("GRAY_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gray")))
        .unwrap_or_else(|| PathBuf::from(".gray"))
}

fn global_file(home: &Path) -> PathBuf {
    home.join("learn").join("global.md")
}

fn project_file(home: &Path, cwd: &str) -> PathBuf {
    let hash = sha1_hex(cwd.as_bytes());
    home.join("learn")
        .join("projects")
        .join(format!("{}.md", &hash[..12]))
}

/// SHA-1 hex digest — dependency-free, used only to key project files.
fn sha1_hex(data: &[u8]) -> String {
    let mut h: [u32; 5] = [0x67452301, 0xEFCDAB89, 0x98BADCFE, 0x10325476, 0xC3D2E1F0];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64) * 8;
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks_exact(64) {
        let mut w = [0u32; 80];
        for (i, word) in chunk.chunks_exact(4).enumerate() {
            w[i] = u32::from_be_bytes([word[0], word[1], word[2], word[3]]);
        }
        for i in 16..80 {
            w[i] = (w[i - 3] ^ w[i - 8] ^ w[i - 14] ^ w[i - 16]).rotate_left(1);
        }
        let (mut a, mut b, mut c, mut d, mut e) = (h[0], h[1], h[2], h[3], h[4]);
        for (i, &wi) in w.iter().enumerate() {
            let (f, k) = match i {
                0..=19 => ((b & c) | ((!b) & d), 0x5A827999u32),
                20..=39 => (b ^ c ^ d, 0x6ED9EBA1),
                40..=59 => ((b & c) | (b & d) | (c & d), 0x8F1BBCDC),
                _ => (b ^ c ^ d, 0xCA62C1D6),
            };
            let t = a
                .rotate_left(5)
                .wrapping_add(f)
                .wrapping_add(e)
                .wrapping_add(k)
                .wrapping_add(wi);
            e = d;
            d = c;
            c = b.rotate_left(30);
            b = a;
            a = t;
        }
        h[0] = h[0].wrapping_add(a);
        h[1] = h[1].wrapping_add(b);
        h[2] = h[2].wrapping_add(c);
        h[3] = h[3].wrapping_add(d);
        h[4] = h[4].wrapping_add(e);
    }
    h.iter().map(|x| format!("{x:08x}")).collect()
}

/// Lesson lines of a file (marker stripped), in order. Missing file → empty.
fn read_lessons(path: &Path) -> Vec<String> {
    std::fs::read_to_string(path)
        .map(|body| {
            body.lines()
                .filter_map(|l| l.trim().strip_prefix("- "))
                .map(str::to_string)
                .filter(|l| !l.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Append `- {text}` to the target file; dedupes identical lines.
fn save_lesson(home: &Path, cwd: &str, text: &str, global: bool) -> Result<String, String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("missing required argument: text".into());
    }
    let path = if global {
        global_file(home)
    } else {
        project_file(home, cwd)
    };
    let line = format!("- {text}");
    let body = std::fs::read_to_string(&path).unwrap_or_default();
    if body.lines().any(|l| l.trim() == line) {
        return Ok(format!("already learned: {text}"));
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| writeln!(f, "{line}"))
        .map_err(|e| format!("can't write {}: {e}", path.display()))?;
    Ok(format!(
        "learned ({scope}): {text}",
        scope = if global { "global" } else { "project" }
    ))
}

/// The `prompt/context` payload: both files, project first, under one
/// header plus the "you may call learn" line. `None` when nothing exists.
fn context_text(home: &Path, cwd: &str) -> Option<String> {
    let project = read_lessons(&project_file(home, cwd));
    let global = read_lessons(&global_file(home));
    if project.is_empty() && global.is_empty() {
        return None;
    }
    let mut out = String::from("## Learned lessons\n");
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
        let cut = out[..CONTEXT_CAP].rfind('\n').unwrap_or(CONTEXT_CAP);
        out.truncate(cut);
        out.push_str("\n- … (truncated)\n");
    }
    out.push_str("\n_You may save new non-obvious lessons with the `learn` tool._");
    Some(out)
}

/// Remove the nth lesson line of the project file (other lines preserved).
fn forget(home: &Path, cwd: &str, n: usize) -> Result<String, String> {
    if n == 0 {
        return Err("forget needs a lesson number ≥ 1".into());
    }
    let path = project_file(home, cwd);
    let body = std::fs::read_to_string(&path)
        .map_err(|_| "no project lessons to forget".to_string())?;
    let mut seen = 0usize;
    let mut removed: Option<String> = None;
    let mut kept: Vec<&str> = Vec::new();
    for line in body.lines() {
        if line.trim_start().starts_with("- ") {
            seen += 1;
            if seen == n {
                removed = Some(line.trim_start()[2..].to_string());
                continue;
            }
        }
        kept.push(line);
    }
    let Some(removed) = removed else {
        return Err(format!("no lesson #{n} — project has {seen}"));
    };
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    std::fs::write(&path, out).map_err(|e| format!("can't write {}: {e}", path.display()))?;
    Ok(format!("forgot: {removed}"))
}

/// Wipe one or both lesson files. `which`: "project", "global", or "all".
fn clear(home: &Path, cwd: &str, which: &str) -> Result<String, String> {
    let mut removed = Vec::new();
    let mut wipe = |path: PathBuf, label: &str| -> Result<(), String> {
        match std::fs::remove_file(&path) {
            Ok(()) => removed.push(label.to_string()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("can't remove {}: {e}", path.display())),
        }
        Ok(())
    };
    match which {
        "project" => wipe(project_file(home, cwd), "project")?,
        "global" => wipe(global_file(home), "global")?,
        "all" => {
            wipe(project_file(home, cwd), "project")?;
            wipe(global_file(home), "global")?;
        }
        other => return Err(format!("clear what? expected project|global|all, got {other:?}")),
    }
    if removed.is_empty() {
        Ok("nothing to clear".into())
    } else {
        Ok(format!("cleared {}", removed.join(" + ")))
    }
}

/// `/learn …` — `argv` excludes the command name. `{"text"}` says it,
/// `{"prompt"}` runs it as a user prompt.
fn run_command(home: &Path, cwd: &str, argv: &[&str]) -> Value {
    match argv.first().copied() {
        None => {
            let project = read_lessons(&project_file(home, cwd));
            let global = read_lessons(&global_file(home));
            if project.is_empty() && global.is_empty() {
                return json!({ "text": format!(
                    "no lessons yet — the agent saves them with the `learn` tool\n\
                     project file: {}\nglobal file: {}",
                    project_file(home, cwd).display(),
                    global_file(home).display()
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
                    project_file(home, cwd).display(),
                    list(&project)
                ));
                if project.len() > 40 {
                    text.push_str(&format!("  … {} more\n", project.len() - 40));
                }
            }
            if !global.is_empty() {
                text.push_str(&format!(
                    "global lessons ({}):\n{}\n",
                    global_file(home).display(),
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
                Some(n) => forget(home, cwd, n),
                None => Err("usage: /learn forget <n>".into()),
            };
            json!({ "text": res.unwrap_or_else(|e| e) })
        }
        Some("clear") => {
            let which = argv.get(1).copied().unwrap_or("project");
            json!({ "text": clear(home, cwd, which).unwrap_or_else(|e| e) })
        }
        Some("distill") => json!({ "prompt":
            "Review what you did and learned in this session. Save anything worth keeping \
             via the `learn` tool: verified build/test quirks, user preferences, pitfalls you \
             hit, non-obvious codebase facts. Use scope \"global\" only when a lesson applies \
             to every project. Skip facts obvious from the code and never save secrets."
        }),
        _ => json!({ "text": format!(
            "learn {} — the agent's lesson memory. /learn · /learn forget <n> · \
             /learn clear [project|global|all] · /learn distill",
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
fn handle(req: &Value) -> (Option<Value>, bool) {
    let id = req.get("id").cloned();
    let method = req.get("method").and_then(Value::as_str).unwrap_or("");
    let params = req.get("params").cloned().unwrap_or(Value::Null);
    let Some(id) = id else {
        return (None, method == "plugin/shutdown");
    };
    let result = match method {
        "plugin/manifest" => manifest(),
        "tool/call" => {
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("args").cloned().unwrap_or(Value::Null);
            let text = match name {
                "learn" => {
                    let text = args.get("text").and_then(Value::as_str).unwrap_or("");
                    let global = args.get("scope").and_then(Value::as_str) == Some("global");
                    match args.get("scope").and_then(Value::as_str) {
                        Some(s) if s != "project" && s != "global" => {
                            Err(format!("bad scope {s:?} — expected \"project\" or \"global\""))
                        }
                        _ => save_lesson(&gray_home(), &req_cwd(&params), text, global),
                    }
                }
                other => Err(format!("unknown tool: {other}")),
            };
            match text {
                Ok(t) => json!({ "content": t }),
                Err(t) => json!({ "content": t, "is_error": true }),
            }
        }
        "command/run" => {
            let argv: Vec<&str> = params
                .get("argv")
                .and_then(Value::as_array)
                .map(|a| a.iter().filter_map(Value::as_str).collect())
                .unwrap_or_default();
            run_command(&gray_home(), &req_cwd(&params), &argv)
        }
        "prompt/context" => match context_text(&gray_home(), &req_cwd(&params)) {
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

fn main() -> std::io::Result<()> {
    if std::env::args().nth(1).as_deref() == Some("manifest") {
        println!("{}", manifest());
        return Ok(());
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = line?;
        let Ok(req) = serde_json::from_str::<Value>(&line) else { continue };
        let (reply, exit) = handle(&req);
        if let Some(reply) = reply {
            writeln!(stdout, "{reply}")?;
            stdout.flush()?;
        }
        if exit {
            break;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(method: &str, params: Value) -> Value {
        handle(&json!({ "id": 1, "method": method, "params": params })).0.unwrap()
    }

    /// A fresh temp "gray home" per test — no env mutation needed.
    fn tmp_home(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gray-learn-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn manifest_claims_tool_command_and_context_hook() {
        let m = call("plugin/manifest", Value::Null)["result"].clone();
        assert_eq!(m["name"], "learn");
        assert_eq!(m["commands"], json!(["/learn"]));
        assert_eq!(m["tools"][0]["name"], "learn");
        assert_eq!(m["hooks"], json!(["prompt/context"]));
        let desc = m["tools"][0]["description"].as_str().unwrap();
        assert!(desc.contains("WHEN to save") && desc.contains("NEVER save"));
    }

    #[test]
    fn sha1_matches_known_vectors() {
        assert_eq!(
            sha1_hex(b"abc"),
            "a9993e364706816aba3e25717850c26c9cd0d89d"
        );
        assert_eq!(
            sha1_hex(b""),
            "da39a3ee5e6b4b0d3255bfef95601890afd80709"
        );
    }

    #[test]
    fn project_file_uses_first_12_hex_of_sha1() {
        let home = Path::new("/h");
        let p = project_file(home, "/work/repo");
        let expected = &sha1_hex(b"/work/repo")[..12];
        assert_eq!(
            p,
            home.join("learn/projects").join(format!("{expected}.md"))
        );
    }

    #[test]
    fn save_appends_and_dedups() {
        let home = tmp_home("dedup");
        save_lesson(&home, "/r", "cargo needs --locked here", false).unwrap();
        let dup = save_lesson(&home, "/r", "  cargo   needs --locked here  ", false).unwrap();
        assert!(dup.starts_with("already learned"));
        assert_eq!(
            read_lessons(&project_file(&home, "/r")),
            vec!["cargo needs --locked here"]
        );
    }

    #[test]
    fn global_and_project_files_are_separate() {
        let home = tmp_home("scope");
        save_lesson(&home, "/r", "everywhere", true).unwrap();
        save_lesson(&home, "/r", "just here", false).unwrap();
        assert_eq!(read_lessons(&global_file(&home)), vec!["everywhere"]);
        assert_eq!(read_lessons(&project_file(&home, "/r")), vec!["just here"]);
    }

    #[test]
    fn context_is_empty_then_project_first_then_global() {
        let home = tmp_home("ctx");
        assert!(context_text(&home, "/r").is_none());
        save_lesson(&home, "/r", "proj lesson", false).unwrap();
        save_lesson(&home, "/r", "glob lesson", true).unwrap();
        let text = context_text(&home, "/r").unwrap();
        assert!(text.starts_with("## Learned lessons\n"));
        assert!(text.find("proj lesson").unwrap() < text.find("glob lesson").unwrap());
        assert!(text.contains("`learn` tool"));
    }

    #[test]
    fn forget_removes_nth_project_line() {
        let home = tmp_home("forget");
        save_lesson(&home, "/r", "one", false).unwrap();
        save_lesson(&home, "/r", "two", false).unwrap();
        save_lesson(&home, "/r", "three", false).unwrap();
        assert_eq!(forget(&home, "/r", 2).unwrap(), "forgot: two");
        assert_eq!(read_lessons(&project_file(&home, "/r")), vec!["one", "three"]);
        assert!(forget(&home, "/r", 3).is_err());
        assert!(forget(&home, "/r", 0).is_err());
    }

    #[test]
    fn clear_wipes_files() {
        let home = tmp_home("clear");
        save_lesson(&home, "/r", "a", false).unwrap();
        save_lesson(&home, "/r", "b", true).unwrap();
        assert_eq!(clear(&home, "/r", "all").unwrap(), "cleared project + global");
        assert!(context_text(&home, "/r").is_none());
        assert!(clear(&home, "/r", "bogus").is_err());
    }

    #[test]
    fn distill_returns_a_prompt_not_text() {
        let home = tmp_home("distill");
        let r = run_command(&home, "/r", &["distill"]);
        assert!(r["prompt"].as_str().unwrap().contains("learn"));
        assert!(r.get("text").is_none());
    }

    #[test]
    fn tool_call_errors_are_flagged() {
        let r = call(
            "tool/call",
            json!({ "name": "learn", "args": {}, "session": {"cwd": "/r"} }),
        );
        assert_eq!(r["result"]["is_error"], true);
        let r = call(
            "tool/call",
            json!({ "name": "learn", "args": {"text": "x", "scope": "nope"} }),
        );
        assert_eq!(r["result"]["is_error"], true);
    }

    #[test]
    fn unknown_methods_are_method_not_found() {
        assert_eq!(call("nope", Value::Null)["error"]["code"], -32601);
    }

    #[test]
    fn shutdown_replies_then_exits_and_notifications_are_silent() {
        let (reply, exit) = handle(&json!({ "id": 2, "method": "plugin/shutdown" }));
        assert!(reply.is_some() && exit);
        let (reply, exit) = handle(&json!({ "method": "plugin/shutdown" }));
        assert!(reply.is_none() && exit);
    }
}
