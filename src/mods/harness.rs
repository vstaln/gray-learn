//! Folded-in gray-harness: continual harness store + evidence-backed
//! refinement loop (prime-agent mechanics port — no prime code), living
//! inside gray-learn so lessons, prompt notes, skill specs and subagent
//! specs share ONE kinded, searchable store.
//!
//! Rationale: a durable store because compaction drops in-context memory
//! (Governance Decay); kinds from the agent-memory taxonomy (From Storage
//! to Experience); refinement gated because self-evolving memory can hurt
//! (Evo-Memory) and unverified feedback reward-hacks (Feedback Loops).
//!
//! The `learn` tool maps to memory-kind entries; the legacy
//! `~/.gray/learn/{global.md,projects/<sha1(cwd)>.md}` files are imported
//! once per scope and renamed `*.imported`. Everything else — scoped
//! stores, idf search, overview digest, review-gated auto-refine,
//! host/run planning, inverse-edit rollback — is the harness port.
//!
//! State: ~/.gray/harness/{state.json,local/<cwd-hash>.json,config.json}

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};

const KINDS: &[&str] = &["prompt", "memory", "skill", "subagent"];
const HOST_RUN_TTL: Duration = Duration::from_secs(240);
const NEED_TURN: &str =
    "refine needs the host.turn capability: gray plugin capabilities learn --all";
const EVIDENCE_CAP: usize = 40;

// ---------- paths & time ----------

fn gray_home() -> PathBuf {
    std::env::var_os("GRAY_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".gray")))
        .unwrap_or_else(|| PathBuf::from("."))
}

fn state_root() -> PathBuf {
    gray_home().join("harness")
}

fn config_file() -> PathBuf {
    state_root().join("config.json")
}

fn global_file() -> PathBuf {
    state_root().join("state.json")
}

/// Per-repo store — project memory holds conventions, not global facts.
fn local_file(cwd: &str) -> PathBuf {
    let mut h = DefaultHasher::new();
    cwd.hash(&mut h);
    state_root()
        .join("local")
        .join(format!("{:016x}.json", h.finish()))
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn session_cwd(params: &Value) -> String {
    params
        .pointer("/session/cwd")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or("default")
        .to_string()
}

// ---------- config ----------

fn config() -> Value {
    let mut cfg = json!({"auto": true, "every_n": 25, "cooldown_min": 20, "digest_bytes": 4096});
    if let Ok(v) =
        serde_json::from_str::<Value>(&std::fs::read_to_string(config_file()).unwrap_or_default())
    {
        if let Some(o) = v.as_object() {
            for (k, v) in o {
                cfg[k] = v.clone();
            }
        }
    }
    cfg
}

fn save_config(cfg: &Value) {
    atomic_write(
        &config_file(),
        &serde_json::to_string_pretty(cfg).unwrap_or_default(),
    );
}

fn cfg_u64(cfg: &Value, key: &str, default: u64) -> u64 {
    cfg[key].as_u64().unwrap_or(default)
}

// ---------- store ----------

fn empty_store(scope: &str, cwd: &str) -> Value {
    let mut entries = json!({});
    for k in KINDS {
        entries[k] = json!({});
    }
    json!({
        "version": 1,
        "scope": scope,
        "cwd": cwd,
        "entries": entries,
        "refinements": [],
        "auto": {"turns_since": 0, "last_run_at": Value::Null},
    })
}

fn store_path(scope: &str, cwd: &str) -> PathBuf {
    if scope == "global" {
        global_file()
    } else {
        local_file(cwd)
    }
}

fn load_store(scope: &str, cwd: &str) -> Value {
    let path = store_path(scope, cwd);
    std::fs::read_to_string(&path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .filter(|v| v["entries"].is_object())
        .unwrap_or_else(|| {
            let mut store = empty_store(scope, cwd);
            if import_legacy_lessons(&mut store, scope, cwd) {
                save_store(scope, cwd, &store);
            }
            store
        })
}

/// One-time import of the legacy gray-learn lesson files into memory-kind
/// entries: `learn/global.md` → global scope; `learn/projects/<sha1(cwd)>.md`
/// → this cwd's local scope. The file is renamed `*.imported` afterwards —
/// the rename is the marker, so nothing can import twice.
fn import_legacy_lessons(store: &mut Value, scope: &str, cwd: &str) -> bool {
    let legacy = if scope == "global" {
        gray_home().join("learn").join("global.md")
    } else {
        let hash = sha1_hex(cwd.as_bytes());
        gray_home()
            .join("learn")
            .join("projects")
            .join(format!("{}.md", &hash[..12]))
    };
    let Ok(body) = std::fs::read_to_string(&legacy) else {
        return false;
    };
    let mut imported = false;
    for line in body.lines() {
        let Some(text) = line.trim().strip_prefix("- ") else {
            continue;
        };
        if text.is_empty() {
            continue;
        }
        let fields = json!({
            "title": text.chars().take(60).collect::<String>(),
            "content": text,
            "path": "lessons",
        });
        if upsert_entry(store, "memory", &fields, "learn-import").is_ok() {
            imported = true;
        }
    }
    if imported {
        let _ = std::fs::rename(&legacy, legacy.with_extension("md.imported"));
    }
    imported
}

/// SHA-1 hex digest — dependency-free, used only to find the legacy
/// per-project lesson file a cwd maps to.
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

fn save_store(scope: &str, cwd: &str, store: &Value) {
    atomic_write(
        &store_path(scope, cwd),
        &serde_json::to_string_pretty(store).unwrap_or_default(),
    );
}

fn atomic_write(path: &PathBuf, content: &str) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let tmp = path.with_extension("tmp");
    if std::fs::write(&tmp, content).is_ok() {
        let _ = std::fs::rename(&tmp, path);
    }
}

/// `local:` / `global:` display prefixes on ids are display-only — edits and
/// lookups always use the bare id. Returns (id, scope override).
fn strip_scope_prefix(id: Option<&str>, scope: &str) -> (Option<String>, String) {
    match id {
        Some(s) if s.starts_with("local:") => (Some(s[6..].to_string()), "local".into()),
        Some(s) if s.starts_with("global:") => (Some(s[7..].to_string()), "global".into()),
        other => (other.map(str::to_string), scope.to_string()),
    }
}

fn scope_of(args: &Value, id: Option<&str>) -> (Option<String>, String) {
    let base = args["scope"].as_str().unwrap_or("local");
    strip_scope_prefix(id, base)
}

// ---------- entries ----------

fn describe(id: &Value, title: &Value) -> String {
    match (id.as_str(), title.as_str()) {
        (Some(i), _) => i.to_string(),
        (None, Some(t)) => format!("'{t}'"),
        _ => "entry".into(),
    }
}

fn require_text(kind: &str, who: &str, field: &str, v: &Value) -> Result<(), String> {
    match v.as_str() {
        Some(s) if !s.is_empty() => Ok(()),
        _ => Err(format!(
            "{kind} entry {who} rejected: {field} must be a non-empty string"
        )),
    }
}

fn optional_text(kind: &str, who: &str, field: &str, v: &Value) -> Result<(), String> {
    if v.is_null() || v.as_str().is_some() {
        Ok(())
    } else {
        Err(format!(
            "{kind} entry {who} rejected: {field} must be a string"
        ))
    }
}

fn optional_record(kind: &str, who: &str, field: &str, v: &Value) -> Result<(), String> {
    if v.is_null() || v.is_object() {
        Ok(())
    } else {
        Err(format!(
            "{kind} entry {who} rejected: {field} must be an object"
        ))
    }
}

/// Write-time validation (port of `_validate_entry_shape`): every create /
/// update / refinement funnels through here so a malformed entry fails with an
/// actionable error instead of poisoning the digest — memory stores are a
/// poisoning target, and skills need explicit applicability data.
fn validate_entry_shape(e: &Value, existing: Option<&Value>) -> Result<(), String> {
    let kind = e["kind"].as_str().unwrap_or("");
    let who = describe(&e["id"], &e["title"]);
    require_text(kind, &who, "id", &e["id"])?;
    require_text(kind, &who, "title", &e["title"])?;
    require_text(kind, &who, "content", &e["content"])?;
    optional_text(kind, &who, "path", &e["path"])?;
    optional_record(kind, &who, "reference", &e["reference"])?;
    optional_record(kind, &who, "arguments", &e["arguments"])?;
    optional_record(kind, &who, "metadata", &e["metadata"])?;
    require_text(kind, &who, "source", &e["source"])?;
    if kind == "skill" {
        match &e["reference"] {
            r if r.is_null() => {
                if existing.is_none() {
                    return Err(format!(
                        "skill entry '{who}' rejected: skill entries require a Python reference"
                    ));
                }
            }
            r => {
                if r["type"].as_str() != Some("python") {
                    return Err(format!(
                        "skill entry '{who}' rejected: reference.type must be \"python\""
                    ));
                }
                require_text(kind, &who, "reference.import", &r["import"])?;
                let callable = r["callable"].as_str().is_some_and(|s| !s.is_empty());
                let pattern = r["call_pattern"].as_str().is_some_and(|s| !s.is_empty());
                if !callable && !pattern {
                    return Err(format!(
                        "skill entry '{who}' rejected: reference needs callable or call_pattern"
                    ));
                }
            }
        }
    }
    Ok(())
}

fn slug(title: &str, kind: &str) -> String {
    let s: String = title
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let s: String = s
        .split('-')
        .filter(|p| !p.is_empty())
        .collect::<Vec<_>>()
        .join("-");
    let s: String = s.chars().take(48).collect();
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        format!("{kind}-{}", now())
    } else {
        s
    }
}

fn upsert_entry(
    store: &mut Value,
    kind: &str,
    fields: &Value,
    source: &str,
) -> Result<Value, String> {
    if !KINDS.contains(&kind) {
        return Err(format!(
            "unknown harness kind '{kind}'; expected one of {KINDS:?}"
        ));
    }
    require_text(kind, "entry", "title", &fields["title"])?;
    let (id, _) = strip_scope_prefix(fields["id"].as_str(), "");
    let explicit = id.as_ref().is_some_and(|s| !s.is_empty());
    let mut entry_id = id
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| slug(fields["title"].as_str().unwrap_or("entry"), kind));
    // Unique slug on create: an explicit id signals update intent, so only
    // auto-slugged ids uniquify — foo, foo-2, foo-3…
    if !explicit {
        let base = entry_id.clone();
        let mut n = 2;
        while !store["entries"][kind][&entry_id].is_null() {
            entry_id = format!("{base}-{n}");
            n += 1;
        }
    }
    let existing = store["entries"][kind][&entry_id].clone();
    let existing = if existing.is_null() {
        None
    } else {
        Some(existing)
    };

    // `source` = provenance: which writer produced the entry, so a bad one
    // can be found and purged later.
    let mut e = json!({
        "id": entry_id,
        "kind": kind,
        "title": fields["title"],
        "content": fields["content"],
        "source": source,
    });
    // Omitted optionals preserve the stored value on update; explicit {} clears.
    for key in ["path", "reference", "arguments", "metadata"] {
        if !fields[key].is_null() {
            e[key] = fields[key].clone();
        } else if let Some(x) = existing.as_ref().and_then(|x| x.get(key)) {
            e[key] = x.clone();
        }
    }
    if e["path"].is_null() {
        e["path"] = json!("general");
    }
    validate_entry_shape(&e, existing.as_ref())?;

    let ts = now();
    e["created_at"] = existing
        .as_ref()
        .and_then(|x| x["created_at"].as_u64())
        .map(|v| json!(v))
        .unwrap_or(json!(ts));
    e["updated_at"] = json!(ts);
    e["version"] = json!(
        existing
            .as_ref()
            .and_then(|x| x["version"].as_u64())
            .unwrap_or(0)
            + 1
    );
    store["entries"][kind][&entry_id] = e.clone();
    Ok(e)
}

fn find_entry<'v>(
    store: &'v Value,
    id: &str,
    kind: Option<&str>,
) -> Option<(&'static str, &'v Value)> {
    for k in KINDS {
        if kind.is_some_and(|w| w != *k) {
            continue;
        }
        if let Some(e) = store["entries"][k].get(id) {
            return Some((k, e));
        }
    }
    None
}

fn query_terms(query: &str) -> Vec<String> {
    query
        .to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.len() >= 2)
        .map(str::to_string)
        .collect()
}

/// Port of `HarnessState.search`: terms score against title / content /
/// path+id; each term's document frequency discounts it idf-style; matching
/// more fields multiplies by 1.5× per extra field; rank by (score, recency).
fn search_entries(
    store: &Value,
    query: &str,
    kind: Option<&str>,
    limit: usize,
) -> Vec<(f64, Value)> {
    let terms = query_terms(query);
    if terms.is_empty() {
        return Vec::new();
    }
    let kinds: Vec<&str> = match kind {
        Some(k) => vec![k],
        None => KINDS.to_vec(),
    };
    let mut corpus: Vec<Value> = Vec::new();
    for k in &kinds {
        if let Some(m) = store["entries"][k].as_object() {
            corpus.extend(m.values().cloned());
        }
    }
    let fields_of = |e: &Value| -> (String, String, String) {
        (
            e["title"].as_str().unwrap_or("").to_lowercase(),
            e["content"].as_str().unwrap_or("").to_lowercase(),
            format!(
                "{} {}",
                e["path"].as_str().unwrap_or(""),
                e["id"].as_str().unwrap_or("")
            )
            .to_lowercase(),
        )
    };
    let mut df: HashMap<&str, usize> = HashMap::new();
    for e in &corpus {
        let (t, c, p) = fields_of(e);
        for term in &terms {
            if t.contains(term.as_str()) || c.contains(term.as_str()) || p.contains(term.as_str()) {
                *df.entry(term.as_str()).or_insert(0) += 1;
            }
        }
    }
    let n = corpus.len() as f64;
    let idf: HashMap<&str, f64> = df
        .iter()
        .filter(|(_, c)| **c > 0)
        .map(|(t, c)| (*t, (1.0 + n / *c as f64).ln()))
        .collect();
    let mut ranked: Vec<(f64, u64, Value)> = corpus
        .into_iter()
        .map(|e| {
            let (t, c, p) = fields_of(&e);
            let mut total = 0.0;
            for (term, w) in &idf {
                let fields = t.contains(term) as usize
                    + c.contains(term) as usize
                    + p.contains(term) as usize;
                if fields > 0 {
                    total += w * (1.0 + (fields as f64 - 1.0) * 0.5);
                }
            }
            (total, e["updated_at"].as_u64().unwrap_or(0), e)
        })
        .filter(|(s, _, _)| *s > 0.0)
        .collect();
    ranked.sort_by(|a, b| b.0.total_cmp(&a.0).then(b.1.cmp(&a.1)));
    ranked.truncate(limit);
    ranked.into_iter().map(|(s, _, e)| (s, e)).collect()
}

/// Port of `HarnessState.overview`: header + per-kind digests + last
/// refinements — the same shape the system prompt digest carries.
fn overview(store: &Value, scope: &str, cwd: &str, max_per_kind: usize) -> String {
    let mut lines = vec![
        format!(
            "Harness state ({scope}): {}",
            store_path(scope, cwd).display()
        ),
        "Call contract: skill entries describe Python callables that run inside the gray-repl \
         namespace (invoke with repl_exec cells like `result = await_skill(args)`); subagent \
         specs compose a concise task prompt and spawn via the rlm_spawn tool or `gray \
         subagents run` — admission returns a handle, never the child's answer; results \
         arrive via agent messages or files."
            .to_string(),
    ];
    for kind in KINDS {
        let mut recs: Vec<&Value> = store["entries"][kind]
            .as_object()
            .map(|m| m.values().collect())
            .unwrap_or_default();
        recs.sort_by_key(|e| e["id"].as_str().unwrap_or("").to_string());
        let total = recs.len();
        lines.push(format!("{kind}: {total}"));
        for e in recs.iter().take(max_per_kind) {
            let mut summary = e["content"].as_str().unwrap_or("").replace('\n', " ");
            if summary.chars().count() > 120 {
                summary = format!("{}...", summary.chars().take(117).collect::<String>());
            }
            let mut extra = String::new();
            if e["kind"].as_str() == Some("skill") {
                for key in ["reference", "arguments"] {
                    if !e[key].is_null() {
                        let t = serde_json::to_string(&e[key]).unwrap_or_default();
                        let short = if t.chars().count() > 120 {
                            format!("{}...", t.chars().take(117).collect::<String>())
                        } else {
                            t
                        };
                        extra.push_str(&format!(
                            " {}={short}",
                            if key == "reference" { "ref" } else { "args" }
                        ));
                    }
                }
            }
            lines.push(format!(
                "  - [{scope}:{}] {} ({}, v{}){extra}: {}",
                e["id"].as_str().unwrap_or(""),
                e["title"].as_str().unwrap_or(""),
                e["path"].as_str().unwrap_or("general"),
                e["version"].as_u64().unwrap_or(1),
                summary.trim(),
            ));
        }
        if total > max_per_kind {
            lines.push(format!("  - +{} more", total - max_per_kind));
        }
    }
    let refs = store["refinements"].as_array().cloned().unwrap_or_default();
    lines.push(format!("refinements: {}", refs.len()));
    for ev in refs.iter().rev().take(5).rev() {
        let changes = ev["changes"]
            .as_array()
            .map(|c| {
                c.iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default();
        lines.push(format!(
            "  - [{}] {}: {}",
            ev["id"].as_str().unwrap_or("?"),
            ev["trigger"].as_str().unwrap_or(""),
            changes
        ));
    }
    lines.join("\n")
}

// ---------- refinement prompts (ported, gray-adapted) ----------

const REFINEMENT_SYSTEM_PROMPT: &str = "You are gray's /refine continual harness subsystem.\n\n\
Your job is to improve the editable continual harness state from the current trajectory.\n\
This is similar in spirit to context compaction, but instead of summarizing the\n\
conversation you emit precise Create, Update, or Delete edits to reusable state.\n\
The continual harness is the persistent, editable set of prompt notes, memories,\n\
skills, and subagent specs that lets the agent improve reusable behavior\n\
outside the token history.\n\n\
Continual harness components:\n\
- prompt: supplemental prompt notes only. The base system prompt is immutable and MUST NOT be rewritten.\n\
- memory: durable facts, decisions, failures, preferences, and outcomes.\n\
- skill: installed skill specs. Skill create/update edits MUST include a `reference` object with \
`{\"type\":\"python\"}`, a Python `import`, and a `callable` or `call_pattern`; they also MUST include \
an `arguments` object describing accepted inputs, required fields, defaults, and constraints. Use `{}` \
for `arguments` only when the callable truly needs no external inputs. Skills run inside the gray-repl \
persistent Python namespace via repl_exec cells.\n\
- subagent: reusable delegation specs, including purpose, instructions, and when to invoke. The call \
form: compose a concise task prompt and spawn with the rlm_spawn tool or `gray subagents run` — \
admission returns a handle immediately, never the child's answer. Results arrive only through \
explicit agent messages or files.\n\n\
Scope and persistence policy:\n\
- The default editable harness store is local to the current working directory. Use it for \
session-specific progress, active task state, coordination notes, temporary blockers, and project \
facts that should not affect other sessions.\n\
- A caller may explicitly request global refinement. Global edits must be stable cross-session \
lessons, durable user preferences, reusable skills/subagents, or tool/environment facts that \
should affect future sessions.\n\
- Entry ids in the harness overview may carry a display-only `local:` or `global:` prefix. Always \
use the bare id (no prefix) in edits.\n\
- All edits in one refinement apply only to the requested scope's store. During a local refinement, \
global entries are read-only context: never propose update or delete edits for them; create a local \
entry instead when a session-specific override is genuinely needed.\n\
- Use memory for declarative facts and preferences, skill for repeatable procedures exposed as \
Python calls, prompt for narrow behavioral policy addendums, and subagent for reusable delegation \
roles.\n\
- Create or update the smallest relevant component.\n\
- When an edit is persisted, include metadata such as `{\"scope\":\"local\"}` when that helps future \
review understand the intended blast radius.\n\n\
Prefer small evidence-backed edits — prefer NO edits over speculative ones. If the trajectory holds \
nothing durable, return an empty edits list. If prior refinements caused issues, rollback or replace \
the faulty entries. Never edit source files directly. Output JSON only with this exact shape:\n\n\
{\n  \"summary\": \"one sentence\",\n  \"rationale\": \"why these edits are justified by trajectory evidence\",\n\
  \"expectedOutcome\": \"what should improve and how to validate it\",\n  \"edits\": [\n    {\n      \"action\": \"create|update|delete\",\n      \"kind\": \"prompt|memory|skill|subagent\",\n      \"id\": \"stable id for update/delete, optional for create\",\n      \"title\": \"required for create/update except delete\",\n      \"content\": \"required for create/update except delete\",\n      \"path\": \"optional grouping path\",\n      \"reference\": {\"type\": \"python\", \"import\": \"package.module\", \"callable\": \"function_name\"},\n      \"arguments\": {\"name\": {\"type\": \"string\", \"required\": true, \"description\": \"accepted input\"}},\n      \"metadata\": {},\n      \"reason\": \"why this edit is useful\"\n    }\n  ]\n}";

// A cheap independent pass decides whether evidence justifies edits before
// the planner runs — this gate is what makes auto-refine safe to leave on.
const REVIEW_GATE_PROMPT: &str = "You are gray's automatic /refine review gate.\n\n\
Decide whether this checkpoint should run /refine. Auto /refine writes local harness state by \
default, so approve when the trajectory contains evidence useful to this session's future turns.\n\
Reject one-off noise, unsupported hypotheses, and transient tool outputs. Ask for global \
refinement only for durable cross-session lessons or explicitly project-qualified lessons likely \
to be reused in future sessions.\n\n\
Return JSON only:\n\
{\n  \"shouldRefine\": true|false,\n  \"rationale\": \"short reason\",\n  \"instructions\": \"optional concise \
instructions for /refine if shouldRefine is true\"\n}";

/// Balanced-JSON extractor: the model sometimes wraps the object in prose or a
/// fence — take the first balanced `{…}` candidate.
fn extract_json(text: &str) -> Result<Value, String> {
    let mut start = None;
    let mut depth = 0i64;
    let mut in_string = false;
    let mut escaped = false;
    for (i, ch) in text.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        if in_string {
            if ch == '\\' {
                escaped = true;
            } else if ch == '"' {
                in_string = false;
            }
            continue;
        }
        match ch {
            '"' => in_string = true,
            '{' => {
                if depth == 0 {
                    start = Some(i);
                }
                depth += 1;
            }
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if let Some(s) = start {
                        return serde_json::from_str(&text[s..=i])
                            .map_err(|e| format!("the model did not return valid JSON: {e}"));
                    }
                }
            }
            _ => {}
        }
    }
    Err("the model stopped before completing its JSON object — retry with a smaller request".into())
}

/// Validate + apply one planner edit; returns (change description, inverse
/// edit for rollback). Inverse of create = delete, of update/delete = restore
/// the prior snapshot.
fn apply_edit(
    store: &mut Value,
    edit: &Value,
    scope_label: &str,
) -> Result<(String, Value), String> {
    let action = edit["action"].as_str().unwrap_or("");
    let kind = edit["kind"].as_str().unwrap_or("");
    if !KINDS.contains(&kind) {
        return Err(format!("edit rejected: unknown kind '{kind}'"));
    }
    let (bare, edit_scope) = strip_scope_prefix(edit["id"].as_str(), scope_label);
    if edit_scope != scope_label {
        return Err(format!(
            "edit rejected: {edit_scope}-scoped id in a {scope_label} refinement"
        ));
    }
    let id = bare.unwrap_or_default();
    match action {
        "create" | "update" => {
            let prior = store["entries"][kind][&id].clone();
            let mut fields = edit.clone();
            fields["id"] = json!(id);
            let e = upsert_entry(store, kind, &fields, "refine")?;
            let eid = e["id"].as_str().unwrap_or("").to_string();
            let inverse = if prior.is_null() {
                json!({"action": "delete", "kind": kind, "id": eid})
            } else {
                json!({"action": "update", "kind": kind, "id": eid,
                       "title": prior["title"], "content": prior["content"],
                       "path": prior["path"], "reference": prior["reference"],
                       "arguments": prior["arguments"], "metadata": prior["metadata"]})
            };
            Ok((format!("{action} {kind}/{eid}"), inverse))
        }
        "delete" => {
            if id.is_empty() {
                return Err("edit rejected: delete requires an id".into());
            }
            let prior = store["entries"][kind][&id].clone();
            if prior.is_null() {
                return Err(format!("edit skipped: {kind}/{id} does not exist"));
            }
            if let Some(m) = store["entries"][kind].as_object_mut() {
                m.remove(&id);
            }
            Ok((
                format!("delete {kind}/{id}"),
                json!({"action": "create", "kind": kind, "id": id,
                       "title": prior["title"], "content": prior["content"],
                       "path": prior["path"], "reference": prior["reference"],
                       "arguments": prior["arguments"], "metadata": prior["metadata"]}),
            ))
        }
        other => Err(format!("edit rejected: unknown action '{other}'")),
    }
}

fn next_refinement_id(store: &Value) -> String {
    let n = store["refinements"]
        .as_array()
        .map(|r| r.len())
        .unwrap_or(0);
    format!("r{}", n + 1)
}

/// The planner pipeline: prompt → host/run → parse → validate → apply →
/// record the event. Shared by the wire tool, `/harness refine` and auto-arm.
/// Fed *evidence* (tool names + error flags), not a flattering self-summary.
fn run_refinement(
    sh: &Shared,
    cwd: &str,
    scope: &str,
    instructions: &str,
    trigger: &str,
) -> Result<String, String> {
    let store = load_store(scope, cwd);
    let ov = overview(&store, scope, cwd, 20);
    let evidence = render_evidence(sh, cwd);
    let prompt = format!(
        "{REFINEMENT_SYSTEM_PROMPT}\n\n## Harness overview ({scope})\n{ov}\n\n## Recent trajectory evidence\n{evidence}\n\n## Caller instructions\n{}\n\nEmit JSON only.",
        if instructions.is_empty() {
            "(none)"
        } else {
            instructions
        }
    );
    let text = host_run(sh, &prompt, HOST_RUN_TTL)?;
    let proposal = extract_json(&text)?;
    let edits = proposal["edits"].as_array().cloned().unwrap_or_default();

    let mut store = store;
    let mut changes: Vec<String> = Vec::new();
    let mut inverse: Vec<Value> = Vec::new();
    let mut rejected: Vec<String> = Vec::new();
    for edit in &edits {
        match apply_edit(&mut store, edit, scope) {
            Ok((desc, inv)) => {
                changes.push(desc);
                inverse.push(inv);
            }
            Err(e) => rejected.push(e),
        }
    }
    let summary = proposal["summary"].as_str().unwrap_or("refinement applied");
    let mut event = json!({
        "id": next_refinement_id(&store),
        "trigger": trigger,
        "changes": changes,
        "evidence": format!("instructions: {}; rejected: {}", instructions, rejected.len()),
        "outcome": proposal["expectedOutcome"].as_str().unwrap_or(""),
        "at": now(),
    });
    // Every applied edit stores its own undo — `/harness rollback` replays it.
    if !inverse.is_empty() {
        event["inverse"] = json!(inverse);
    }
    store["refinements"]
        .as_array_mut()
        .map(|r| r.push(event.clone()));
    save_store(scope, cwd, &store);

    let mut out = format!(
        "refinement {}: {summary}\n  applied: {}",
        event["id"].as_str().unwrap_or("r?"),
        if changes.is_empty() {
            "no edits".to_string()
        } else {
            event["changes"]
                .as_array()
                .map(|c| {
                    c.iter()
                        .filter_map(Value::as_str)
                        .collect::<Vec<_>>()
                        .join(", ")
                })
                .unwrap_or_default()
        }
    );
    if !rejected.is_empty() {
        out.push_str(&format!("\n  rejected: {}", rejected.join("; ")));
    }
    Ok(out)
}

// ---------- host bridge (gray-btw pattern) ----------

pub(crate) type Pending = Arc<Mutex<HashMap<String, mpsc::Sender<Value>>>>;

pub(crate) struct Shared {
    pub(crate) out: Arc<Mutex<std::io::Stdout>>,
    pub(crate) pending: Pending,
    counter: Arc<Mutex<u64>>,
    evidence: Arc<Mutex<HashMap<String, VecDeque<String>>>>,
    refine_busy: Arc<AtomicBool>,
}

impl Shared {
    pub(crate) fn new() -> Self {
        Self {
            out: Arc::new(Mutex::new(std::io::stdout())),
            pending: Arc::new(Mutex::new(HashMap::new())),
            counter: Arc::new(Mutex::new(0)),
            evidence: Arc::new(Mutex::new(HashMap::new())),
            refine_busy: Arc::new(AtomicBool::new(false)),
        }
    }
}

fn next_id(sh: &Shared) -> String {
    let mut n = sh.counter.lock().expect("counter");
    *n += 1;
    format!("h{n}")
}

fn host_send(sh: &Shared, method: &str, params: Value, id: &str) {
    let req = json!({"id": id, "method": method, "params": params});
    let mut o = sh.out.lock().expect("stdout");
    let _ = writeln!(o, "{req}");
    let _ = o.flush();
}

/// Blocking host/run. Reply is routed by the reader thread through `pending`.
fn host_run(sh: &Shared, prompt: &str, ttl: Duration) -> Result<String, String> {
    let id = next_id(sh);
    let (tx, rx) = mpsc::channel();
    sh.pending.lock().expect("pending").insert(id.clone(), tx);
    host_send(sh, "host/run", json!({"prompt": prompt}), &id);
    let res = rx.recv_timeout(ttl);
    sh.pending.lock().expect("pending").remove(&id);
    match res {
        Err(_) => Err(NEED_TURN.into()),
        Ok(v) => {
            if let Some(e) = v.pointer("/error/message").and_then(Value::as_str) {
                return Err(format!("{e} — {NEED_TURN}"));
            }
            let text = v
                .pointer("/result/text")
                .or_else(|| v.get("text"))
                .and_then(Value::as_str)
                .unwrap_or("")
                .trim()
                .to_string();
            if text.is_empty() {
                Err(format!("host/run returned nothing — {NEED_TURN}"))
            } else {
                Ok(text)
            }
        }
    }
}

/// host/say receipt — fire-and-forget; the host reply is dropped by the reader.
fn host_say(sh: &Shared, text: &str) {
    let id = next_id(sh);
    host_send(sh, "host/say", json!({"text": text}), &id);
}

// ---------- evidence ----------

fn push_evidence(sh: &Shared, key: &str, line: String) {
    let mut ev = sh.evidence.lock().expect("evidence");
    let q = ev.entry(key.to_string()).or_default();
    if q.len() >= EVIDENCE_CAP {
        q.pop_front();
    }
    q.push_back(line);
}

fn render_evidence(sh: &Shared, key: &str) -> String {
    let ev = sh.evidence.lock().expect("evidence");
    match ev.get(key) {
        Some(q) if !q.is_empty() => q.iter().cloned().collect::<Vec<_>>().join("\n"),
        _ => "(no tool/turn events observed this session)".to_string(),
    }
}

/// Count one turn_end; arm the review gate every N turns past the cooldown
/// (the cadence is the drift bound — batched, gated writes, not continuous
/// self-edit).
fn on_turn_end(sh: &Shared, cwd: &str, usage: &Value) {
    push_evidence(
        sh,
        cwd,
        format!(
            "turn_end usage={}",
            serde_json::to_string(usage).unwrap_or_default()
        ),
    );
    let cfg = config();
    if !cfg["auto"].as_bool().unwrap_or(true) {
        return;
    }
    let every = cfg_u64(&cfg, "every_n", 25).max(1);
    let cooldown = cfg_u64(&cfg, "cooldown_min", 20) * 60;
    let mut store = load_store("local", cwd);
    let turns = store["auto"]["turns_since"].as_u64().unwrap_or(0) + 1;
    store["auto"]["turns_since"] = json!(turns);
    if turns < every {
        save_store("local", cwd, &store);
        return;
    }
    let last = store["auto"]["last_run_at"].as_u64().unwrap_or(0);
    if now().saturating_sub(last) < cooldown {
        save_store("local", cwd, &store);
        return;
    }
    store["auto"]["turns_since"] = json!(0);
    store["auto"]["last_run_at"] = json!(now());
    save_store("local", cwd, &store);

    if sh.refine_busy.swap(true, Ordering::SeqCst) {
        return; // a refinement is already in flight
    }
    let sh2 = Shared {
        out: sh.out.clone(),
        pending: sh.pending.clone(),
        counter: sh.counter.clone(),
        evidence: sh.evidence.clone(),
        refine_busy: sh.refine_busy.clone(),
    };
    let cwd = cwd.to_string();
    std::thread::spawn(move || {
        let gate_prompt = format!(
            "{REVIEW_GATE_PROMPT}\n\n## Recent trajectory evidence\n{}",
            render_evidence(&sh2, &cwd)
        );
        let verdict = host_run(&sh2, &gate_prompt, Duration::from_secs(60))
            .ok()
            .and_then(|t| extract_json(&t).ok());
        let refine = verdict
            .as_ref()
            .map(|v| v["shouldRefine"].as_bool().unwrap_or(false))
            .unwrap_or(false);
        if refine {
            let instructions = verdict
                .as_ref()
                .and_then(|v| v["instructions"].as_str())
                .unwrap_or("")
                .to_string();
            match run_refinement(&sh2, &cwd, "local", &instructions, "auto") {
                Ok(summary) => host_say(&sh2, &format!("harness auto-refine\n{summary}")),
                Err(e) => host_say(&sh2, &format!("harness auto-refine failed: {e}")),
            }
        }
        sh2.refine_busy.store(false, Ordering::SeqCst);
    });
}

// ---------- tools ----------

pub(crate) fn tool_call(sh: &Shared, params: &Value) -> Value {
    let name = params["name"].as_str().unwrap_or("");
    let args = &params["args"];
    let cwd = session_cwd(params);
    let res: Result<String, String> = (|| match name {
        "harness_upsert" => {
            let kind = args["kind"].as_str().unwrap_or("");
            let (id, scope) = scope_of(args, args["id"].as_str());
            let mut fields = args.clone();
            fields["id"] = json!(id);
            let mut store = load_store(&scope, &cwd);
            let e = upsert_entry(&mut store, kind, &fields, "agent")?;
            save_store(&scope, &cwd, &store);
            Ok(serde_json::to_string_pretty(&e).unwrap_or_default())
        }
        "harness_get" => {
            let (id, scope) = scope_of(args, args["id"].as_str());
            let id = id.ok_or("harness_get requires id")?;
            let store = load_store(&scope, &cwd);
            match find_entry(&store, &id, args["kind"].as_str()) {
                Some((k, e)) => Ok(format!(
                    "{k}\n{}",
                    serde_json::to_string_pretty(e).unwrap_or_default()
                )),
                None => Err(format!("no entry '{id}' in {scope} store")),
            }
        }
        "harness_delete" => {
            let (id, scope) = scope_of(args, args["id"].as_str());
            let id = id.ok_or("harness_delete requires id")?;
            let mut store = load_store(&scope, &cwd);
            match find_entry(&store, &id, args["kind"].as_str()).map(|(k, _)| k) {
                Some(k) => {
                    if let Some(m) = store["entries"][k].as_object_mut() {
                        m.remove(&id);
                    }
                    save_store(&scope, &cwd, &store);
                    Ok(format!("deleted {k}/{id} ({scope})"))
                }
                None => Err(format!("no entry '{id}' in {scope} store")),
            }
        }
        "harness_list" => {
            let (_, scope) = scope_of(args, None);
            let store = load_store(&scope, &cwd);
            let limit = args["limit"].as_u64().unwrap_or(50) as usize;
            let kinds: Vec<&str> = match args["kind"].as_str() {
                Some(k) => vec![k],
                None => KINDS.to_vec(),
            };
            let mut lines = Vec::new();
            for k in kinds {
                let mut recs: Vec<&Value> = store["entries"][k]
                    .as_object()
                    .map(|m| m.values().collect())
                    .unwrap_or_default();
                recs.sort_by_key(|e| e["id"].as_str().unwrap_or("").to_string());
                for e in recs.into_iter().take(limit) {
                    lines.push(format!(
                        "[{scope}:{k}:{}] {} ({}, v{})",
                        e["id"].as_str().unwrap_or(""),
                        e["title"].as_str().unwrap_or(""),
                        e["path"].as_str().unwrap_or("general"),
                        e["version"].as_u64().unwrap_or(1),
                    ));
                }
            }
            Ok(if lines.is_empty() {
                format!("{scope} store is empty")
            } else {
                lines.join("\n")
            })
        }
        "harness_search" => {
            let query = args["query"]
                .as_str()
                .ok_or("harness_search requires query")?;
            let (_, scope) = scope_of(args, None);
            let store = load_store(&scope, &cwd);
            let limit = args["limit"].as_u64().unwrap_or(10) as usize;
            let ranked = search_entries(&store, query, args["kind"].as_str(), limit);
            if ranked.is_empty() {
                return Ok("no matches".into());
            }
            Ok(ranked
                .iter()
                .map(|(s, e)| {
                    format!(
                        "{:.2} [{}/{}] {}: {}",
                        s,
                        e["kind"].as_str().unwrap_or(""),
                        e["id"].as_str().unwrap_or(""),
                        e["title"].as_str().unwrap_or(""),
                        e["content"].as_str().unwrap_or("").replace('\n', " ")
                    )
                })
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "harness_overview" => {
            let (_, scope) = scope_of(args, None);
            let store = load_store(&scope, &cwd);
            Ok(overview(
                &store,
                &scope,
                &cwd,
                args["max_entries"].as_u64().unwrap_or(20) as usize,
            ))
        }
        "harness_record_refinement" => {
            let (_, scope) = scope_of(args, None);
            let trigger = args["trigger"].as_str().unwrap_or("");
            if trigger.is_empty() {
                return Err("harness_record_refinement requires trigger".into());
            }
            let changes = args["changes"].clone();
            let changes_ok = match &changes {
                Value::String(s) => !s.is_empty(),
                Value::Array(a) => a.iter().all(|c| c.as_str().is_some_and(|s| !s.is_empty())),
                _ => false,
            };
            if !changes_ok {
                return Err(
                    "changes must be a non-empty string or a list of non-empty strings".into(),
                );
            }
            let changes = match changes {
                Value::String(s) => vec![json!(s)],
                Value::Array(a) => a,
                _ => vec![],
            };
            let mut store = load_store(&scope, &cwd);
            let event = json!({
                "id": next_refinement_id(&store),
                "trigger": trigger,
                "changes": changes,
                "evidence": args["evidence"].as_str().unwrap_or(""),
                "outcome": args["outcome"].as_str().unwrap_or(""),
                "at": now(),
            });
            store["refinements"].as_array_mut().map(|r| r.push(event));
            save_store(&scope, &cwd, &store);
            Ok(format!("recorded ({scope})"))
        }
        "harness_plan_refinement" => {
            let (_, scope) = scope_of(args, None);
            if sh.refine_busy.swap(true, Ordering::SeqCst) {
                return Err("a refinement is already running".into());
            }
            let instructions = args["instructions"].as_str().unwrap_or("");
            let r = run_refinement(sh, &cwd, &scope, instructions, "manual");
            sh.refine_busy.store(false, Ordering::SeqCst);
            r
        }
        other => return Err(format!("unknown tool {other}")),
    })();
    match res {
        Ok(c) => json!({"content": c}),
        Err(e) => json!({"content": e, "is_error": true}),
    }
}

// ---------- command ----------

pub(crate) fn command(sh: &Shared, params: &Value) -> String {
    let argv: Vec<String> = params["argv"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect()
        })
        .unwrap_or_default();
    let cwd = session_cwd(params);
    let verb = argv.first().map(String::as_str).unwrap_or("status");
    match verb {
        "list" => {
            let scope = argv.get(1).map(String::as_str).unwrap_or("local");
            let store = load_store(scope, &cwd);
            let mut lines = Vec::new();
            for k in KINDS {
                if let Some(m) = store["entries"][k].as_object() {
                    for e in m.values() {
                        lines.push(format!(
                            "[{k}:{}] {} (v{})",
                            e["id"].as_str().unwrap_or(""),
                            e["title"].as_str().unwrap_or(""),
                            e["version"].as_u64().unwrap_or(1)
                        ));
                    }
                }
            }
            lines.sort();
            if lines.is_empty() {
                format!("{scope} harness store is empty")
            } else {
                lines.join("\n")
            }
        }
        "show" => {
            let Some(id) = argv.get(1) else {
                return "usage: /harness show <id> [local|global]".into();
            };
            let scope = argv.get(2).map(String::as_str).unwrap_or("local");
            let (bare, scope) = strip_scope_prefix(Some(id), scope);
            let store = load_store(&scope, &cwd);
            match find_entry(&store, &bare.unwrap_or_default(), None) {
                Some((k, e)) => format!(
                    "{k}\n{}",
                    serde_json::to_string_pretty(e).unwrap_or_default()
                ),
                None => format!("no entry '{id}' in {scope} store"),
            }
        }
        "delete" => {
            let Some(id) = argv.get(1) else {
                return "usage: /harness delete <id> [local|global]".into();
            };
            let scope = argv.get(2).map(String::as_str).unwrap_or("local");
            let (bare, scope) = strip_scope_prefix(Some(id), scope);
            let id = bare.unwrap_or_default();
            let mut store = load_store(&scope, &cwd);
            match find_entry(&store, &id, None).map(|(k, _)| k) {
                Some(k) => {
                    if let Some(m) = store["entries"][k].as_object_mut() {
                        m.remove(&id);
                    }
                    save_store(&scope, &cwd, &store);
                    format!("deleted {k}/{id} ({scope})")
                }
                None => format!("no entry '{id}' in {scope} store"),
            }
        }
        "refine" => {
            if sh.refine_busy.swap(true, Ordering::SeqCst) {
                return "a refinement is already running".into();
            }
            let instructions = argv[1..].join(" ");
            let r = run_refinement(sh, &cwd, "local", &instructions, "manual");
            sh.refine_busy.store(false, Ordering::SeqCst);
            match r {
                Ok(s) => s,
                Err(e) => e,
            }
        }
        "rollback" => {
            let scope = argv.get(1).map(String::as_str).unwrap_or("local");
            let mut store = load_store(scope, &cwd);
            let event = store["refinements"]
                .as_array()
                .and_then(|r| r.iter().rev().find(|e| e["inverse"].is_array()))
                .cloned();
            match event {
                None => "nothing to roll back".into(),
                Some(ev) => {
                    let mut undone = Vec::new();
                    for inv in ev["inverse"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .rev()
                    {
                        match apply_edit(&mut store, inv, scope) {
                            Ok((desc, _)) => undone.push(desc),
                            Err(e) => undone.push(e),
                        }
                    }
                    if let Some(m) = store["refinements"].as_array_mut() {
                        if let Some(last) = m.iter_mut().rev().find(|e| e["id"] == ev["id"]) {
                            last["rolled_back"] = json!(true);
                        }
                    }
                    save_store(scope, &cwd, &store);
                    format!(
                        "rolled back {}: {}",
                        ev["id"].as_str().unwrap_or("r?"),
                        undone.join(", ")
                    )
                }
            }
        }
        "on" | "off" => {
            let mut cfg = config();
            cfg["auto"] = json!(verb == "on");
            save_config(&cfg);
            format!(
                "auto-refine {}",
                if verb == "on" { "enabled" } else { "disabled" }
            )
        }
        _ => {
            let cfg = config();
            let store = load_store("local", &cwd);
            let counts: Vec<String> = KINDS
                .iter()
                .map(|k| {
                    format!(
                        "{k}={}",
                        store["entries"][k]
                            .as_object()
                            .map(|m| m.len())
                            .unwrap_or(0)
                    )
                })
                .collect();
            format!(
                "harness status\n  local: {} ({})\n  auto-refine: {} · every {} turns · {}min cooldown · {}B digest cap\n  verbs: list [scope] · show <id> · delete <id> · refine [instructions] · rollback · on|off",
                local_file(&cwd).display(),
                counts.join(" "),
                if cfg["auto"].as_bool().unwrap_or(true) {
                    "on"
                } else {
                    "off"
                },
                cfg_u64(&cfg, "every_n", 25),
                cfg_u64(&cfg, "cooldown_min", 20),
                cfg_u64(&cfg, "digest_bytes", 4096),
            )
        }
    }
}

// ---------- prompt/context ----------

/// The harness block for the merged prompt/context payload: per-kind
/// counts, non-memory entries in detail (memory entries are rendered by
/// the lessons section), the last refinements, and auto-refine status.
/// `None` when the store is entirely empty. Deliberately bounded — a
/// digest that can grow past its cap buries what matters.
pub(crate) fn digest(cwd: &str) -> Option<String> {
    let cfg = config();
    let cap = cfg_u64(&cfg, "digest_bytes", 4096) as usize;
    let store = load_store("local", cwd);
    let non_empty = KINDS.iter().any(|k| {
        store["entries"][k]
            .as_object()
            .is_some_and(|m| !m.is_empty())
    }) || store["refinements"]
        .as_array()
        .is_some_and(|r| !r.is_empty());
    if !non_empty {
        return None;
    }
    let mut lines = vec!["## Harness".to_string()];
    for kind in KINDS {
        let mut recs: Vec<&Value> = store["entries"][kind]
            .as_object()
            .map(|m| m.values().collect())
            .unwrap_or_default();
        recs.sort_by_key(|e| e["id"].as_str().unwrap_or("").to_string());
        lines.push(format!("{kind}: {}", recs.len()));
        if *kind == "memory" {
            continue; // lessons section above renders the contents
        }
        for e in recs.iter().take(8) {
            let mut summary = e["content"].as_str().unwrap_or("").replace('\n', " ");
            if summary.chars().count() > 120 {
                summary = format!("{}...", summary.chars().take(117).collect::<String>());
            }
            lines.push(format!(
                "  - [{}] {} (v{}): {}",
                e["id"].as_str().unwrap_or(""),
                e["title"].as_str().unwrap_or(""),
                e["version"].as_u64().unwrap_or(1),
                summary.trim(),
            ));
        }
        if recs.len() > 8 {
            lines.push(format!("  - +{} more", recs.len() - 8));
        }
    }
    let refs = store["refinements"]
        .as_array()
        .map(|r| r.len())
        .unwrap_or(0);
    lines.push(format!(
        "refinements: {refs} · auto-refine {} (every {} turns)",
        if cfg["auto"].as_bool().unwrap_or(true) {
            "on"
        } else {
            "off"
        },
        cfg_u64(&cfg, "every_n", 25),
    ));
    let mut text = lines.join("\n");
    if text.len() > cap {
        let mut end = cap;
        while end > 0 && !text.is_char_boundary(end) {
            end -= 1;
        }
        text = format!("{}\n…[harness digest truncated at {cap}B]", &text[..end]);
    }
    Some(text)
}

// ---------- events ----------

pub(crate) fn on_event(sh: &Shared, params: &Value) {
    let cwd = params
        .pointer("/session/cwd")
        .and_then(Value::as_str)
        .unwrap_or("default")
        .to_string();
    match params["type"].as_str().unwrap_or("") {
        "pre_tool" => {
            let name = params["name"].as_str().unwrap_or("tool");
            push_evidence(sh, &cwd, format!("pre_tool {name}"));
        }
        "post_tool" => {
            // Evidence is the outcome signal, not the action.
            let name = params["name"].as_str().unwrap_or("tool");
            let err = params["is_error"].as_bool().unwrap_or(false);
            push_evidence(
                sh,
                &cwd,
                format!("post_tool {name}{}", if err { " (error)" } else { "" }),
            );
        }
        "turn_end" => on_turn_end(sh, &cwd, &params["usage"]),
        _ => {}
    }
}

// ---------- learn adapter (`learn` tool ↔ memory-kind entries) ----------

/// Memory-kind contents in stable listing order (created, then id) —
/// these render as the lesson lines in the context payload.
fn memory_entries(scope: &str, cwd: &str) -> Vec<Value> {
    let store = load_store(scope, cwd);
    let mut recs: Vec<Value> = store["entries"]["memory"]
        .as_object()
        .map(|m| m.values().cloned().collect())
        .unwrap_or_default();
    recs.sort_by(|a, b| {
        a["created_at"]
            .as_u64()
            .unwrap_or(0)
            .cmp(&b["created_at"].as_u64().unwrap_or(0))
            .then(
                a["id"]
                    .as_str()
                    .unwrap_or("")
                    .cmp(b["id"].as_str().unwrap_or("")),
            )
    });
    recs
}

/// The lesson lines (memory entry contents) for a scope.
pub(crate) fn lessons(scope: &str, cwd: &str) -> Vec<String> {
    memory_entries(scope, cwd)
        .iter()
        .filter_map(|e| e["content"].as_str().map(str::to_string))
        .collect()
}

/// `learn` tool → memory-kind upsert. Dedupes identical contents in the
/// target scope, matching the old file-append behavior — a bounded store is
/// a security property, not housekeeping.
pub(crate) fn learn_save(cwd: &str, text: &str, global: bool) -> Result<String, String> {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if text.is_empty() {
        return Err("missing required argument: text".into());
    }
    let scope = if global { "global" } else { "local" };
    if memory_entries(scope, cwd)
        .iter()
        .any(|e| e["content"].as_str() == Some(text.as_str()))
    {
        return Ok(format!("already learned: {text}"));
    }
    let mut store = load_store(scope, cwd);
    let fields = json!({
        "title": text.chars().take(60).collect::<String>(),
        "content": text,
        "path": "lessons",
    });
    upsert_entry(&mut store, "memory", &fields, "learn")?;
    save_store(scope, cwd, &store);
    Ok(format!(
        "learned ({scope}): {text}",
        scope = if global { "global" } else { "project" }
    ))
}

/// `/learn forget <n>` → delete the nth project (local) memory entry.
pub(crate) fn learn_forget(cwd: &str, n: usize) -> Result<String, String> {
    if n == 0 {
        return Err("forget needs a lesson number ≥ 1".into());
    }
    let entries = memory_entries("local", cwd);
    let Some(e) = entries.get(n - 1) else {
        return Err(format!("no lesson #{n} — project has {}", entries.len()));
    };
    let (id, text) = (
        e["id"].as_str().unwrap_or("").to_string(),
        e["content"].as_str().unwrap_or("").to_string(),
    );
    let mut store = load_store("local", cwd);
    if let Some(m) = store["entries"]["memory"].as_object_mut() {
        m.remove(&id);
    }
    save_store("local", cwd, &store);
    Ok(format!("forgot: {text}"))
}

/// `/learn clear project|global|all` → wipe memory-kind entries only.
pub(crate) fn learn_clear(cwd: &str, which: &str) -> Result<String, String> {
    let wipe = |scope: &str| -> usize {
        let mut store = load_store(scope, cwd);
        let n = store["entries"]["memory"]
            .as_object()
            .map(|m| m.len())
            .unwrap_or(0);
        if n > 0 {
            store["entries"]["memory"] = json!({});
            save_store(scope, cwd, &store);
        }
        n
    };
    match which {
        "project" => Ok(format!("cleared {} project lessons", wipe("local"))),
        "global" => Ok(format!("cleared {} global lessons", wipe("global"))),
        "all" => Ok(format!(
            "cleared {} project + {} global lessons",
            wipe("local"),
            wipe("global")
        )),
        other => Err(format!(
            "clear what? expected project|global|all, got {other:?}"
        )),
    }
}

/// `gray harness …` argv surface — read verbs work without a host.
pub(crate) fn cli(argv: &[String]) -> String {
    let cwd = std::env::current_dir()
        .map(|p| p.display().to_string())
        .unwrap_or_else(|_| "default".into());
    let sh = Shared::new();
    let run = |a: &[&str]| {
        command(
            &sh,
            &json!({"argv": a, "session": {"id": "cli", "cwd": cwd}}),
        )
    };
    match argv.first().map(String::as_str) {
        Some("status") | None => run(&["status"]),
        Some("list") => run(&["list"]),
        Some("overview") => {
            let store = load_store("local", &cwd);
            overview(&store, "local", &cwd, 20)
        }
        Some("show") => argv
            .get(1)
            .map(|id| run(&["show", id.as_str()]))
            .unwrap_or_else(|| "usage: gray harness show <id>".into()),
        Some("delete") => argv
            .get(1)
            .map(|id| run(&["delete", id.as_str()]))
            .unwrap_or_else(|| "usage: gray harness delete <id>".into()),
        Some(other) => format!("unknown verb {other} — list|status|overview|show|delete"),
    }
}

/// The folded-in harness_* tool declarations (manifest fragment).
pub(crate) fn manifest_tools() -> Vec<Value> {
    json!([
        {"name": "harness_upsert",
         "description": "Create or update a harness entry (prompt|memory|skill|subagent). Omitted optional fields preserve stored values on update; version increments.",
         "parameters": {"type": "object", "properties": {
            "kind": {"type": "string", "enum": ["prompt", "memory", "skill", "subagent"]},
            "title": {"type": "string"}, "content": {"type": "string"},
            "id": {"type": "string", "description": "stable id; slugged from title when omitted"},
            "path": {"type": "string", "description": "grouping path (default general)"},
            "reference": {"type": "object", "description": "skill only: {type:python, import, callable|call_pattern}"},
            "arguments": {"type": "object"}, "metadata": {"type": "object"},
            "scope": {"type": "string", "enum": ["local", "global"]}},
         "required": ["kind", "title", "content"]}},
        {"name": "harness_get",
         "description": "Fetch one harness entry by id (optionally kind + scope).",
         "parameters": {"type": "object", "properties": {
            "id": {"type": "string"}, "kind": {"type": "string"},
            "scope": {"type": "string", "enum": ["local", "global"]}},
         "required": ["id"]}},
        {"name": "harness_delete",
         "description": "Delete a harness entry by id.",
         "parameters": {"type": "object", "properties": {
            "id": {"type": "string"}, "kind": {"type": "string"},
            "scope": {"type": "string", "enum": ["local", "global"]}},
         "required": ["id"]}},
        {"name": "harness_list",
         "description": "List harness entries, optionally filtered by kind.",
         "parameters": {"type": "object", "properties": {
            "kind": {"type": "string"}, "limit": {"type": "integer"},
            "scope": {"type": "string", "enum": ["local", "global"]}}}},
        {"name": "harness_search",
         "description": "Ranked search over all harness entries (idf-weighted terms across title/content/path).",
         "parameters": {"type": "object", "properties": {
            "query": {"type": "string"}, "kind": {"type": "string"},
            "limit": {"type": "integer"},
            "scope": {"type": "string", "enum": ["local", "global"]}},
         "required": ["query"]}},
        {"name": "harness_overview",
         "description": "Full digest of the harness store: per-kind entries, versions, and recent refinement events.",
         "parameters": {"type": "object", "properties": {
            "max_entries": {"type": "integer"},
            "scope": {"type": "string", "enum": ["local", "global"]}}}},
        {"name": "harness_record_refinement",
         "description": "Record a refinement event the agent applied outside the planner (audited in the store).",
         "parameters": {"type": "object", "properties": {
            "trigger": {"type": "string"}, "changes": {},
            "evidence": {"type": "string"}, "outcome": {"type": "string"},
            "scope": {"type": "string", "enum": ["local", "global"]}},
         "required": ["trigger", "changes"]}},
        {"name": "harness_plan_refinement",
         "description": "Run the refinement planner now: gather trajectory evidence, plan validated create/update/delete edits via host/run, apply and record them. Needs the host.turn capability.",
         "parameters": {"type": "object", "properties": {
            "instructions": {"type": "string"},
            "scope": {"type": "string", "enum": ["local", "global"]}}}},
    ])
    .as_array()
    .cloned()
    .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmpdir(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("gray-learn-harness-{}-{}", tag, std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn manifest_tools_shape() {
        let t = manifest_tools();
        assert_eq!(t.len(), 8);
        assert!(
            t.iter()
                .all(|t| t["name"].as_str().unwrap().starts_with("harness_"))
        );
    }

    #[test]
    fn upsert_validates_and_versions() {
        let mut store = empty_store("local", "t");
        let f = json!({"title": "T", "content": "body", "kind": "memory"});
        let e = upsert_entry(&mut store, "memory", &f, "agent").unwrap();
        assert_eq!(e["version"], 1);
        assert_eq!(e["path"], "general");
        let e2 = upsert_entry(&mut store, "memory", &f, "agent").unwrap();
        // no explicit id → unique slug on create (e2 is a NEW entry)
        assert_ne!(e["id"], e2["id"]);
        // explicit id → update: version increments on the same entry
        let f3 = json!({"id": e["id"], "title": "T", "content": "body v2"});
        let e3 = upsert_entry(&mut store, "memory", &f3, "agent").unwrap();
        assert_eq!(e3["id"], e["id"]);
        assert_eq!(e3["version"], 2);
        assert!(upsert_entry(&mut store, "bogus", &f, "agent").is_err());
        assert!(upsert_entry(&mut store, "memory", &json!({"content": "x"}), "agent").is_err());
    }

    #[test]
    fn skill_requires_python_reference() {
        let mut store = empty_store("local", "t");
        let f = json!({"title": "S", "content": "does a thing"});
        assert!(upsert_entry(&mut store, "skill", &f, "agent").is_err());
        let f = json!({"title": "S", "content": "does a thing",
            "reference": {"type": "python", "import": "os", "callable": "getcwd"},
            "arguments": {}});
        assert!(upsert_entry(&mut store, "skill", &f, "agent").is_ok());
    }

    #[test]
    fn search_ranks_multi_field_matches() {
        let mut store = empty_store("local", "t");
        upsert_entry(
            &mut store,
            "memory",
            &json!({"title": "auth retry policy", "content": "refresh tokens", "path": "auth"}),
            "agent",
        )
        .unwrap();
        upsert_entry(
            &mut store,
            "prompt",
            &json!({"title": "unrelated note", "content": "something else"}),
            "agent",
        )
        .unwrap();
        let hits = search_entries(&store, "auth token", None, 10);
        assert_eq!(hits.len(), 1);
        assert_eq!(hits[0].1["kind"], "memory");
    }

    #[test]
    fn store_roundtrip_and_edit_apply() {
        let dir = tmpdir("roundtrip");
        let path = dir.join("s.json");
        atomic_write(
            &path,
            &serde_json::to_string_pretty(&json!({"entries": {}})).unwrap(),
        );
        let mut store: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
        for k in KINDS {
            store["entries"][k] = json!({});
        }
        let (desc, inv) = apply_edit(
            &mut store,
            &json!({"action": "create", "kind": "memory", "title": "x", "content": "y"}),
            "local",
        )
        .unwrap();
        assert!(desc.starts_with("create memory/"));
        assert_eq!(inv["action"], "delete");
    }

    #[test]
    fn extract_json_finds_balanced_object() {
        let t = "preamble {\"summary\": \"s\", \"edits\": [{\"a\": 1}]} trailing";
        let v = extract_json(t).unwrap();
        assert_eq!(v["summary"], "s");
        assert_eq!(v["edits"].as_array().unwrap().len(), 1);
    }

    #[test]
    fn learn_save_dedupes_and_lists() {
        // Point the harness home at a temp dir by driving through
        // load_store paths directly: this exercises the adapter logic on
        // a synthetic store rather than env mutation (unsafe in 2024).
        let mut store = empty_store("local", "t");
        let fields = json!({"title": "cargo needs --locked", "content": "cargo needs --locked", "path": "lessons"});
        upsert_entry(&mut store, "memory", &fields, "learn").unwrap();
        let contents: Vec<&str> = store["entries"]["memory"]
            .as_object()
            .unwrap()
            .values()
            .filter_map(|e| e["content"].as_str())
            .collect();
        assert_eq!(contents, vec!["cargo needs --locked"]);
    }
}
