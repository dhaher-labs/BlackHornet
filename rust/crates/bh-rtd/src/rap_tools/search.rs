//! Native `search.web` — the Rust mirror of
//! `src/agent_tools/search.py#SearchTool` (PLAN.md §4 Phase 4).
//!
//! The native server implements the **path-based** actions, exactly as
//! scoped by Phase 4 ("search over path"):
//!
//! * **`local`** — walk `path` (default `.` → the workspace sandbox root),
//!   filter by `file_pattern` (the peer's `fnmatch`), then match the query
//!   case-insensitively: file-name hits first (`relevance: 0.9`,
//!   `type: filename_match`), then content hits in files < 100,000 bytes
//!   (`relevance: 0.7`, `type: content_match`, first matching line only,
//!   snippet clipped to 200 chars). `max_results` (default 10) stops the
//!   walk exactly like the peer's `break`s.
//! * **`code`** — walk `path`, skip hidden dirs and
//!   `node_modules`/`__pycache__`/`.git`/`venv`/`.venv`, keep the peer's
//!   code-extension set, match each line against the query: literal
//!   substring by default (case-insensitive unless `case_sensitive`), or a
//!   regex when `regex: true`. Hits carry
//!   `{file, line, match, extension}` metadata and 300-char snippets.
//! * **`web`** — NOT executed natively, and not fabricated either. The
//!   Python implementation is an openly-labelled *simulation* (fabricated
//!   `example.com` results); a native mirror would be a fabrication with
//!   no simulation disclaimer, so the action returns the peer's
//!   failure shape with an explanatory `error` instead.
//! * **Sandbox** — `path` (default `.`) is resolved under the workspace
//!   root like `file`; a root outside the sandbox returns the peer's
//!   single-error-row contract with `Path outside the workspace sandbox:
//!   ...` — the walk never leaves the root.
//! * **Empty query** → the peer's `{results: [], total: 0, error: "Empty
//!   query"}` shape; a missing root directory → the peer's single
//!   `{title: "Error", snippet: "Path does not exist: ..."}` result entry
//!   (that really is its contract); unknown action → the peer's empty
//!   result list.
//!
//! Result rows are `SearchResult.to_dict()` verbatim:
//! `{title, url, snippet, source, relevance, metadata}` with relevance
//! rounded to 3 decimals; the wrapper adds `total`, `action`, `query` and
//! `duration_ms` (rounded to 2).
//!
//! Documented delta: `regex: true` (code action) is refused loudly — the
//! std-only build has no regex engine, and half-mirroring one would be
//! fabrication. The default (`regex: false`) covers the peer's common
//! path natively.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::file::{fnmatch, resolve_base_dir};
use super::round2;

/// Files above this size are not content-searched (`_local_search`).
pub const MAX_CONTENT_SEARCH_BYTES: u64 = 100_000;

/// The peer's code-file extension set (`_code_search`).
pub const CODE_EXTENSIONS: [&str; 30] = [
    ".py", ".js", ".ts", ".tsx", ".jsx", ".go", ".rs", ".java", ".c", ".cpp", ".h", ".hpp",
    ".rb", ".php", ".swift", ".kt", ".sh", ".bash", ".zsh", ".sql", ".html", ".css", ".yaml",
    ".yml", ".json", ".toml", ".cfg", ".ini", ".md", ".rst",
];

/// Directories the code search skips (`_code_search`).
pub const SKIPPED_DIRS: [&str; 5] = ["node_modules", "__pycache__", ".git", "venv", ".venv"];

/// Default `max_results` (`input_schema` default).
pub const DEFAULT_MAX_RESULTS: usize = 10;

/// The native `search.web` engine (sandboxed to `base_dir`).
#[derive(Clone, Debug)]
pub struct SearchEngine {
    base_dir: PathBuf,
}

impl SearchEngine {
    /// Build an engine sandboxed to `base_dir`.
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: resolve_base_dir(base_dir.to_path_buf()),
        }
    }

    /// Execute one `search.web` tool call (`args` is the tool's JSON
    /// object; see the module docs for the mirrored contract).
    ///
    /// # Errors
    ///
    /// [`super::NativeToolError::InvalidParams`] when `action` or `query`
    /// is missing — the tool's required-parameter contract, enforced
    /// loudly (the Python tool would `KeyError` into the registry's
    /// generic failure).
    pub fn execute(&self, args: &Value) -> Result<Value, super::NativeToolError> {
        let start = std::time::Instant::now();
        let Some(action) = args.get("action").and_then(Value::as_str) else {
            return Err(super::NativeToolError::InvalidParams(
                "search.web requires string 'action'".into(),
            ));
        };
        let Some(query) = args.get("query").and_then(Value::as_str) else {
            return Err(super::NativeToolError::InvalidParams(
                "search.web requires string 'query'".into(),
            ));
        };

        if query.trim().is_empty() {
            return Ok(json!({
                "results": [],
                "total": 0,
                "action": action,
                "query": query,
                "error": "Empty query",
            }));
        }

        let results: Vec<Value> = match action {
            "web" => {
                // Honest refusal: the peer's `web` action is a labelled
                // simulation; the native server neither executes nor
                // fabricates it.
                return Ok(json!({
                    "results": [],
                    "total": 0,
                    "action": action,
                    "query": query,
                    "error": "action 'web' is a simulated Python-side implementation \
                              and is not executed by the native server",
                }));
            }
            "local" => self.local_search(args, query),
            "code" => self.code_search(args, query),
            // The peer's `execute` returns `[]` for unknown actions.
            _ => Vec::new(),
        };

        Ok(json!({
            "results": results,
            "total": results.len(),
            "action": action,
            "query": query,
            "duration_ms": round2(start.elapsed().as_secs_f64() * 1000.0),
        }))
    }

    /// The root directory for this call: `args.path` (default `.`) resolved
    /// under the sandbox. An escaping path is refused with the peer's
    /// error-row contract — the walk never leaves the sandbox.
    fn resolve_root(&self, args: &Value) -> Result<PathBuf, String> {
        let raw = args.get("path").and_then(Value::as_str).unwrap_or(".");
        let candidate = Path::new(raw);
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.base_dir.join(candidate)
        };
        let resolved = super::file::normalize_lexical(&joined);
        if !resolved.starts_with(&self.base_dir) {
            return Err(format!("Path outside the workspace sandbox: {raw}"));
        }
        if let Ok(canon) = resolved.canonicalize() {
            let canon_base = self
                .base_dir
                .canonicalize()
                .unwrap_or_else(|_| self.base_dir.clone());
            if !canon.starts_with(&canon_base) {
                return Err(format!("Path outside the workspace sandbox: {raw}"));
            }
            return Ok(canon);
        }
        Ok(resolved)
    }

    /// `_local_search` (minus the sandbox-note: the root must stay under
    /// the sandbox, so an escaping `path` yields the peer's
    /// "Path does not exist" entry rather than a walk outside).
    fn local_search(&self, args: &Value, query: &str) -> Vec<Value> {
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_RESULTS);
        let file_pattern = args
            .get("file_pattern")
            .and_then(Value::as_str)
            .unwrap_or("*");
        let root = match self.resolve_root(args) {
            Ok(root) => root,
            Err(message) => return vec![error_result("local", &message)],
        };

        let q_lower = query.to_ascii_lowercase();
        let mut results: Vec<Value> = Vec::new();
        let mut entries: Vec<PathBuf> = Vec::new();
        collect_walk(&root, false, &mut entries);
        if entries.is_empty() && !root.is_dir() {
            // The peer's exact "missing root" contract.
            return vec![error_result("local", &format!("Path does not exist: {}", root.display()))];
        }
        for filepath in entries {
            if results.len() >= max_results {
                break;
            }
            let Some(filename) = filepath.file_name().map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            if !fnmatch(&filename, file_pattern) {
                continue;
            }

            // Filename match first (`continue` skips the content check).
            if filename.to_ascii_lowercase().contains(&q_lower) {
                results.push(json!({
                    "title": filename,
                    "url": filepath.to_string_lossy(),
                    "snippet": format!("File name matches '{query}'"),
                    "source": "local",
                    "relevance": 0.9,
                    "metadata": {"type": "filename_match"},
                }));
                continue;
            }

            // Content match: small files only, first matching line.
            let Ok(meta) = fs::metadata(&filepath) else {
                continue;
            };
            if meta.len() >= MAX_CONTENT_SEARCH_BYTES {
                continue;
            }
            let Ok(content) = fs::read_to_string(&filepath) else {
                continue; // the peer ignores unreadable/undecodable files
            };
            for (line_num, line) in content.lines().enumerate() {
                if line.to_ascii_lowercase().contains(&q_lower) {
                    results.push(json!({
                        "title": filename,
                        "url": filepath.to_string_lossy(),
                        "snippet": clip(line.trim(), 200),
                        "source": "local",
                        "relevance": 0.7,
                        "metadata": {"type": "content_match", "line": line_num + 1},
                    }));
                    break; // first content match per file, like the peer
                }
            }
        }
        results.truncate(max_results);
        results
    }

    /// `_code_search` (literal mode native; regex mode refused loudly).
    fn code_search(&self, args: &Value, query: &str) -> Vec<Value> {
        let max_results = args
            .get("max_results")
            .and_then(Value::as_u64)
            .map(|n| n as usize)
            .unwrap_or(DEFAULT_MAX_RESULTS);
        let case_sensitive = args
            .get("case_sensitive")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let use_regex = args
            .get("regex")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let root = match self.resolve_root(args) {
            Ok(root) => root,
            Err(message) => return vec![error_result("code", &message)],
        };

        let mut entries: Vec<PathBuf> = Vec::new();
        collect_walk(&root, true, &mut entries);
        if entries.is_empty() && !root.is_dir() {
            return vec![error_result("code", &format!("Path does not exist: {}", root.display()))];
        }

        if use_regex {
            // Documented delta: no std regex engine — refuse loudly, never
            // approximate.
            return vec![error_result(
                "code",
                "regex search is not executed by the native server \
                 (use the Python tool path or regex=false)",
            )];
        }
        let needle = if case_sensitive {
            query.to_owned()
        } else {
            query.to_ascii_lowercase()
        };

        let mut results: Vec<Value> = Vec::new();
        for filepath in entries {
            if results.len() >= max_results {
                break;
            }
            let Some(filename) = filepath.file_name().map(|n| n.to_string_lossy().into_owned())
            else {
                continue;
            };
            let Some(ext) = filename.rsplit_once('.').map(|(_, e)| format!(".{}", e.to_ascii_lowercase())) else {
                continue;
            };
            if !CODE_EXTENSIONS.contains(&ext.as_str()) {
                continue;
            }
            let Ok(content) = fs::read_to_string(&filepath) else {
                continue; // the peer ignores unreadable files
            };
            for (idx, line) in content.lines().enumerate() {
                let haystack = if case_sensitive {
                    line.to_owned()
                } else {
                    line.to_ascii_lowercase()
                };
                if let Some(matched) = find_match(&haystack, &needle) {
                    results.push(json!({
                        "title": format!("{filename}:{}", idx + 1),
                        "url": filepath.to_string_lossy(),
                        "snippet": clip(line.trim(), 300),
                        "source": "code",
                        "relevance": 0.8,
                        "metadata": {
                            "file": filepath.to_string_lossy(),
                            "line": idx + 1,
                            "match": matched,
                            "extension": ext,
                        },
                    }));
                    if results.len() >= max_results {
                        break;
                    }
                }
            }
        }
        results.truncate(max_results);
        results
    }
}

/// Find the matched substring for the result's `metadata.match` field
/// (the peer records `match.group(0)`; for the literal mirror that is the
/// needle itself, in the haystack's case form).
fn find_match(haystack: &str, needle: &str) -> Option<String> {
    if haystack.contains(needle) {
        Some(needle.to_owned())
    } else {
        None
    }
}

/// The peer's missing-root contract: one `Error` result row.
fn error_result(source: &str, message: &str) -> Value {
    json!({
        "title": "Error",
        "url": "",
        "snippet": message,
        "source": source,
        "relevance": 0.0,
        "metadata": {},
    })
}

/// Clip a snippet to `max` chars (Python's `[:n]` on `str` is chars).
fn clip(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// Walk `root` collecting files (and only files). `code_mode` applies the
/// code search's directory filters (hidden dirs + the skip set); `local`
/// mode walks everything. Sorted per directory for deterministic output.
fn collect_walk(root: &Path, code_mode: bool, out: &mut Vec<PathBuf>) {
    if !root.is_dir() {
        return;
    }
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut children: Vec<PathBuf> = match fs::read_dir(&dir) {
            Ok(iter) => iter.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
            Err(_) => continue,
        };
        children.sort();
        for child in children {
            let name = child
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let is_symlink_dir = child.is_dir()
                && fs::symlink_metadata(&child)
                    .map(|m| m.file_type().is_symlink())
                    .unwrap_or(false);
            if child.is_dir() && !is_symlink_dir {
                if code_mode
                    && (name.starts_with('.') || SKIPPED_DIRS.contains(&name.as_str()))
                {
                    continue;
                }
                stack.push(child);
            } else if child.is_file() {
                out.push(child);
            }
        }
    }
}

/// The `search.web` input schema — the verbatim transcription of
/// `SearchTool.input_schema()`.
pub fn input_schema() -> Value {
    json!({
        "type": "object",
        "required": ["action", "query"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["web", "local", "code"],
                "description": "Search type",
            },
            "query": {
                "type": "string",
                "description": "Search query string",
            },
            "max_results": {
                "type": "integer",
                "default": 10,
                "description": "Maximum results to return",
            },
            "provider": {
                "type": "string",
                "default": "duckduckgo",
                "description": "Web search provider (simulated)",
            },
            "path": {
                "type": "string",
                "description": "Root path for local/code search",
            },
            "file_pattern": {
                "type": "string",
                "default": "*",
                "description": "Glob pattern to filter files (local/code)",
            },
            "case_sensitive": {
                "type": "boolean",
                "default": false,
                "description": "Case-sensitive search (code)",
            },
            "regex": {
                "type": "boolean",
                "default": false,
                "description": "Treat query as regex (code)",
            },
        },
    })
}

/// The `search.web` output schema — the verbatim transcription of
/// `SearchTool.output_schema()`.
pub fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "results": {"type": "array"},
            "total": {"type": "integer"},
            "action": {"type": "string"},
            "query": {"type": "string"},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct Sandbox {
        root: PathBuf,
    }

    impl Sandbox {
        fn new(tag: &str) -> Self {
            let root = std::env::temp_dir().join(format!(
                "bh-rap-search-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&root).expect("mkdir sandbox");
            Self { root }
        }

        fn engine(&self) -> SearchEngine {
            SearchEngine::new(&self.root)
        }

        fn call(&self, args: Value) -> Value {
            self.engine().execute(&args).expect("search tool executes")
        }

        fn write(&self, rel: &str, content: &str) {
            let path = self.root.join(rel);
            fs::create_dir_all(path.parent().unwrap()).expect("mkdir");
            fs::write(path, content).expect("write");
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn local_search_finds_names_then_content() {
        let sandbox = Sandbox::new("local");
        sandbox.write("src/alpha_handler.py", "def run():\n    return 'alpha-token'\n");
        sandbox.write("docs/beta.md", "the alpha-token hides here\n");
        sandbox.write("skip.bin", "alpha-token but wrong extension pattern\n");

        // Filename hit (relevance 0.9) comes from `alpha_handler.py`.
        let result = sandbox.call(json!({"action": "local", "query": "alpha", "file_pattern": "*.py"}));
        assert_eq!(result["total"], 1);
        let row = &result["results"][0];
        assert_eq!(row["title"], "alpha_handler.py");
        assert_eq!(row["source"], "local");
        assert_eq!(row["relevance"], 0.9);
        assert_eq!(row["metadata"]["type"], "filename_match");

        // Content hits across the walk (relevance 0.7, line numbers). The
        // peer's default `file_pattern` is `*`, so `skip.bin` counts too —
        // three content rows, one per file (first matching line each).
        let result = sandbox.call(json!({"action": "local", "query": "alpha-token"}));
        assert_eq!(result["total"], 3);
        for row in result["results"].as_array().expect("rows") {
            assert_eq!(row["metadata"]["type"], "content_match");
            assert!(row["metadata"]["line"].as_u64().expect("line") >= 1);
        }
        assert!(result["results"][0]["snippet"]
            .as_str()
            .expect("snippet")
            .contains("alpha-token"));
    }

    #[test]
    fn local_search_respects_max_results_and_missing_root_contract() {
        let sandbox = Sandbox::new("localmax");
        for i in 0..5 {
            sandbox.write(&format!("f{i}.txt"), &format!("needle {i}\n"));
        }
        let result = sandbox.call(json!({"action": "local", "query": "needle", "max_results": 3}));
        assert_eq!(result["total"], 3);

        let result = sandbox.call(json!({"action": "local", "query": "x", "path": "ghost_dir"}));
        assert_eq!(result["total"], 1);
        assert_eq!(result["results"][0]["title"], "Error");
        assert!(result["results"][0]["snippet"]
            .as_str()
            .expect("snippet")
            .starts_with("Path does not exist: "));
    }

    #[test]
    fn code_search_filters_extensions_and_dirs() {
        let sandbox = Sandbox::new("code");
        sandbox.write("src/app.py", "x = 1\nTARGET = 'here'\ny = 2\n");
        sandbox.write("src/app.rs", "let target = 1;\n");
        sandbox.write("notes.txt", "TARGET in a non-code file\n");
        sandbox.write("node_modules/pkg.js", "TARGET in node_modules\n");
        sandbox.write(".hidden/target.py", "TARGET hidden\n");
        sandbox.write("src/app.js", "let TARGET = 'js';\n");

        let result = sandbox.call(json!({"action": "code", "query": "target", "path": "."}));
        let rows = result["results"].as_array().expect("rows");
        let files: Vec<&str> = rows
            .iter()
            .map(|r| r["url"].as_str().expect("url"))
            .collect();
        assert!(files.iter().any(|f| f.ends_with("app.py")), "{files:?}");
        assert!(files.iter().any(|f| f.ends_with("app.rs")), "{files:?}");
        assert!(files.iter().any(|f| f.ends_with("app.js")), "{files:?}");
        assert!(!files.iter().any(|f| f.contains("node_modules")), "{files:?}");
        assert!(!files.iter().any(|f| f.contains(".hidden")), "{files:?}");
        assert!(!files.iter().any(|f| f.ends_with("notes.txt")), "{files:?}");
        // Title carries `file:line`, metadata the 1-based line number.
        let py_row = rows.iter().find(|r| r["url"].as_str().unwrap().ends_with("app.py")).unwrap();
        assert_eq!(py_row["title"], "app.py:2");
        assert_eq!(py_row["metadata"]["line"], 2);
        assert_eq!(py_row["metadata"]["extension"], ".py");
        assert_eq!(py_row["source"], "code");
        assert_eq!(py_row["relevance"], 0.8);
    }

    #[test]
    fn code_search_case_sensitivity_and_regex_refusal() {
        let sandbox = Sandbox::new("codecase");
        sandbox.write("a.py", "lower target\nUPPER TARGET\n");

        let result = sandbox.call(json!({"action": "code", "query": "TARGET", "path": "."}));
        assert_eq!(result["total"], 2, "default is case-insensitive");

        let result = sandbox.call(json!({
            "action": "code", "query": "TARGET", "path": ".", "case_sensitive": true,
        }));
        assert_eq!(result["total"], 1);
        assert!(result["results"][0]["snippet"]
            .as_str()
            .expect("snippet")
            .contains("UPPER"));

        // `regex: true` is refused loudly (documented delta), not faked.
        let result = sandbox.call(json!({
            "action": "code", "query": "t.rget", "path": ".", "regex": true,
        }));
        assert_eq!(result["total"], 1);
        assert!(result["results"][0]["snippet"]
            .as_str()
            .expect("snippet")
            .contains("not executed by the native server"));
    }

    #[test]
    fn web_action_is_refused_not_fabricated() {
        let sandbox = Sandbox::new("web");
        let result = sandbox.call(json!({"action": "web", "query": "rust runtime"}));
        assert_eq!(result["results"], json!([]));
        assert_eq!(result["total"], 0);
        let error = result["error"].as_str().expect("error");
        assert!(error.contains("simulated Python-side"), "got: {error}");
    }

    #[test]
    fn empty_query_and_unknown_action_mirror_the_peer() {
        let sandbox = Sandbox::new("empty");
        let result = sandbox.call(json!({"action": "local", "query": "   "}));
        assert_eq!(result["error"], "Empty query");
        assert_eq!(result["total"], 0);

        // Unknown action → the peer's empty result list.
        let result = sandbox.call(json!({"action": "astral", "query": "x"}));
        assert_eq!(result["total"], 0);
        assert_eq!(result["results"], json!([]));
    }

    #[test]
    fn missing_required_params_are_typed_errors() {
        let sandbox = Sandbox::new("params");
        assert!(matches!(
            sandbox.engine().execute(&json!({"action": "local"})),
            Err(super::super::NativeToolError::InvalidParams(_))
        ));
        assert!(matches!(
            sandbox.engine().execute(&json!({"query": "x"})),
            Err(super::super::NativeToolError::InvalidParams(_))
        ));
    }

    #[test]
    fn search_stays_inside_the_workspace_sandbox() {
        let sandbox = Sandbox::new("sandbox");
        // A path outside the sandbox is refused with the error-row
        // contract; the walk never leaves the root.
        let result = sandbox.call(json!({
            "action": "local", "query": "anything", "path": "/etc",
        }));
        assert_eq!(result["results"][0]["title"], "Error");
        assert!(result["results"][0]["snippet"]
            .as_str()
            .expect("snippet")
            .starts_with("Path outside the workspace sandbox: /etc"));

        // Traversal syntax is refused the same way.
        let result = sandbox.call(json!({
            "action": "code", "query": "x", "path": "../..",
        }));
        assert!(result["results"][0]["snippet"]
            .as_str()
            .expect("snippet")
            .contains("outside the workspace sandbox"));
    }
}
