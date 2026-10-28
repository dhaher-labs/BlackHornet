//! Native `file` — the Rust mirror of
//! `src/agent_tools/file_tool.py#FileTool` (PLAN.md §4 Phase 4).
//!
//! Mirrored semantics (from the Python source):
//!
//! * **Sandbox** (`_validate_path`): every path — including `copy`/`move`
//!   destinations — is resolved against the engine's base directory
//!   (absolute, lexically normalized, and symlink-followed via
//!   `canonicalize` when the target exists). Anything escaping the base
//!   directory is refused with the peer's own business failure
//!   `Path traversal detected: <path>` — never executed, never resolved
//!   "somewhere plausible".
//! * **Operations**: `read`, `write`, `edit`, `list`, `delete`, `mkdir`,
//!   `copy`, `move`, `glob`, `diff`, `patch` — same dispatch, same argument
//!   names, same result payloads (`{success, output, error?, metadata?}`),
//!   same messages (`File not found: ...`, `Path is a directory, use 'list'
//!   operation: ...`, `Replaced N occurrence(s) ...`, `Created directory:
//!   ...`, ...).
//! * **Limits**: the 50 MiB read size limit (`max_file_size` default), the
//!   500-entry list/glob caps with `... and N more ...`, glob recursion
//!   default `true`, edit's first-occurrence-only default.
//! * **Encoding detection** (`detect_encoding`): BOM-first (`utf-8-sig`,
//!   `utf-16-le`, `utf-16-be`), then a strict UTF-8 decode, then the
//!   lossy fallback (`errors="replace"` equivalent). Documented delta: the
//!   Python cascade also names legacy codecs (`latin-1`, `gbk`, ...); files
//!   that are only decodable under those land on the peer's own
//!   last-resort branch here (`utf-8 (fallback)` with replacement chars).
//! * **Fail loud**: IO failures carry the peer's `Failed to <op>: <cause>`
//!   business-failure shape; missing params are the peer's own
//!   `ToolResult(success=False)` failures; nothing degrades to an empty
//!   success.
//!
//! Documented deltas (post-MVP honesty, not faked):
//!
//! * `diff` renders a correct unified diff of the same two texts with an
//!   LCS-based algorithm; hunk *layout* may differ from Python's
//!   `difflib` (its `SequenceMatcher` heuristics are not mirrored
//!   byte-for-byte). Sides larger than [`DIFF_MAX_LINES`] lines are refused
//!   loudly instead of silently degrading.
//! * `patch` mirrors the peer's hunk application algorithm verbatim —
//!   including its looseness (hunk context is not verified) — because
//!   mirroring means mirroring.

use std::fs;
use std::io::Write as _;
use std::path::{Component, Path, PathBuf};

use serde_json::{json, Value};

/// `max_file_size_mb` default from `FileTool.__init__` (50 MiB).
pub const MAX_FILE_SIZE_BYTES: u64 = 50 * 1024 * 1024;

/// The 500-entry output cap shared by `list` and `glob`
/// (`entries[:500]` / `matches[:500]`).
pub const OUTPUT_ENTRY_CAP: usize = 500;

/// Upper bound for the native `diff` algorithm (lines per side). Above this
/// the tool refuses loudly (see the module docs' delta note).
pub const DIFF_MAX_LINES: usize = 1500;

/// One `file` tool call against the sandbox rooted at `base_dir`.
#[derive(Clone, Debug)]
pub struct FileEngine {
    base_dir: PathBuf,
}

impl FileEngine {
    /// Build an engine sandboxed to `base_dir`.
    pub fn new(base_dir: &Path) -> Self {
        Self {
            base_dir: resolve_base_dir(base_dir.to_path_buf()),
        }
    }

    /// Execute one `file` tool call (`args` is the tool's JSON object).
    /// Business outcomes (including every failure the Python tool returns
    /// or raises) travel in the tool's own result payload; this never
    /// returns `Err` for tool-level outcomes.
    pub fn execute(&self, args: &Value) -> Result<Value, super::NativeToolError> {
        let operation = args.get("operation").and_then(Value::as_str).unwrap_or("");
        let path_str = args.get("path").and_then(Value::as_str).unwrap_or("");

        if operation.is_empty() {
            return Ok(fail("No operation specified"));
        }
        if path_str.is_empty() {
            return Ok(fail("No path specified"));
        }

        // `_validate_path` — a traversal is the peer's business failure.
        let path = match self.validate(path_str) {
            Ok(path) => path,
            Err(message) => return Ok(fail(&message)),
        };

        match operation {
            "read" => self.read(&path),
            "write" => self.write(args, &path),
            "edit" => self.edit(args, &path),
            "list" => self.list(args, &path),
            "delete" => self.delete(&path),
            "mkdir" => self.mkdir(&path),
            "copy" => self.copy(args, &path),
            "move" => self.move_op(args, &path),
            "glob" => self.glob(args, &path),
            "diff" => self.diff(args, &path),
            "patch" => self.patch(args, &path),
            other => Ok(fail(&format!("Unknown operation: {other}"))),
        }
    }

    // ── Sandbox (`_validate_path`) ─────────────────────────────────────

    /// Validate and resolve `raw` against the sandbox root. `Err` carries
    /// the peer's traversal message (a business failure, not a panic).
    pub fn validate(&self, raw: &str) -> Result<PathBuf, String> {
        let candidate = Path::new(raw);
        // Python: `(self._base_dir / path)` — an absolute operand replaces
        // the base entirely (pathlib semantics).
        let joined = if candidate.is_absolute() {
            candidate.to_path_buf()
        } else {
            self.base_dir.join(candidate)
        };
        let resolved = normalize_lexical(&joined);
        if !resolved.starts_with(&self.base_dir) {
            return Err(format!("Path traversal detected: {raw}"));
        }
        // Symlink escape check: resolve the deepest EXISTING ancestor (the
        // file itself when present, else its parent chain) and rebuild the
        // path on top of its canonical form — a symlinked parent must not
        // smuggle a write outside the sandbox even when the leaf does not
        // exist yet (this mirrors Python's non-strict `Path.resolve()`,
        // which follows the existing prefix).
        let canon_base = self
            .base_dir
            .canonicalize()
            .unwrap_or_else(|_| self.base_dir.clone());
        let mut probe = resolved.clone();
        let mut missing: Vec<std::ffi::OsString> = Vec::new();
        while !probe.exists() {
            match probe.file_name() {
                Some(name) => {
                    missing.push(name.to_os_string());
                    match probe.parent() {
                        Some(parent) if !parent.as_os_str().is_empty() => {
                            probe = parent.to_path_buf();
                        }
                        _ => break,
                    }
                }
                None => break,
            }
        }
        if let Ok(canon) = probe.canonicalize() {
            let mut full = canon;
            for seg in missing.iter().rev() {
                full.push(seg);
            }
            if !full.starts_with(&canon_base) {
                return Err(format!("Path traversal detected: {raw}"));
            }
            return Ok(full);
        }
        Ok(resolved)
    }

    // ── Operations ─────────────────────────────────────────────────────

    fn read(&self, path: &Path) -> Result<Value, super::NativeToolError> {
        if !path.exists() {
            return Ok(fail(&format!("File not found: {}", path.display())));
        }
        if path.is_dir() {
            return Ok(fail(&format!(
                "Path is a directory, use 'list' operation: {}",
                path.display()
            )));
        }
        let file_size = match fs::metadata(path) {
            Ok(meta) => meta.len(),
            Err(e) => return Ok(tool_execution_failure("read", &e)),
        };
        if file_size > MAX_FILE_SIZE_BYTES {
            return Ok(fail(&format!(
                "File too large: {file_size} bytes (max: {MAX_FILE_SIZE_BYTES})"
            )));
        }
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(e) => return Ok(tool_execution_failure("read", &e)),
        };
        let (content, encoding) = read_with_detection(&raw);
        Ok(json!({
            "success": true,
            "output": content,
            "metadata": {"encoding": encoding, "size_bytes": file_size},
        }))
    }

    fn write(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        let Some(content) = args.get("content").and_then(Value::as_str) else {
            return Ok(fail("No content specified for write operation"));
        };
        if let Err(e) = fs::create_dir_all(path.parent().unwrap_or(Path::new("."))) {
            return Ok(tool_execution_failure("write", &e));
        }
        let write_result =
            fs::File::create(path).and_then(|mut file| file.write_all(content.as_bytes()));
        if let Err(e) = write_result {
            return Ok(tool_execution_failure("write", &e));
        }
        Ok(json!({
            "success": true,
            "output": format!(
                "Successfully wrote {} characters to {}",
                content.chars().count(),
                path.display()
            ),
        }))
    }

    fn edit(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        let old_text = args.get("old_text").and_then(Value::as_str).unwrap_or("");
        let new_text = args.get("new_text").and_then(Value::as_str).unwrap_or("");
        let replace_all = args
            .get("replace_all")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        if old_text.is_empty() {
            return Ok(fail("old_text is required for edit operation"));
        }
        if !path.exists() {
            return Ok(fail(&format!("File not found: {}", path.display())));
        }
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(e) => return Ok(tool_execution_failure("edit", &e)),
        };
        let (content, _) = read_with_detection(&raw);
        if !content.contains(old_text) {
            return Ok(fail("old_text not found in file"));
        }
        let count = content.matches(old_text).count();
        let (new_content, replaced) = if replace_all {
            (content.replace(old_text, new_text), count)
        } else {
            (content.replacen(old_text, new_text, 1), 1)
        };
        if let Err(e) = fs::write(path, new_content.as_bytes()) {
            return Ok(tool_execution_failure("edit", &e));
        }
        Ok(json!({
            "success": true,
            "output": format!(
                "Replaced {replaced} occurrence(s) of old_text in {} ({count} total match(es) found)",
                path.display()
            ),
        }))
    }

    fn list(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        if !path.exists() {
            return Ok(fail(&format!("Path not found: {}", path.display())));
        }
        if !path.is_dir() {
            return Ok(fail(&format!("Path is not a directory: {}", path.display())));
        }
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        let mut entries: Vec<String> = Vec::new();
        if recursive {
            // Mirror `os.walk` top-down: per directory, dirs (sorted) then
            // files (sorted), each rendered relative to the root.
            walk_sorted(path, &mut |rel, is_dir| {
                if is_dir {
                    entries.push(format!("  {rel}/"));
                } else {
                    entries.push(format!("  {rel}"));
                }
            });
        } else {
            let mut children: Vec<PathBuf> = match fs::read_dir(path) {
                Ok(iter) => iter.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
                Err(e) => return Ok(tool_execution_failure("list", &e)),
            };
            children.sort();
            for child in children {
                let is_dir = child.is_dir();
                let suffix = if is_dir { "/" } else { "" };
                let mut size = String::new();
                if !is_dir {
                    if let Ok(meta) = fs::metadata(&child) {
                        size = format!("  ({} bytes)", meta.len());
                    }
                }
                let name = child
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                entries.push(format!("  {name}{suffix}{size}"));
            }
        }

        let entry_count = entries.len();
        let mut output = format!("Contents of {}:\n", path.display());
        output.push_str(&entries[..entry_count.min(OUTPUT_ENTRY_CAP)].join("\n"));
        if entry_count > OUTPUT_ENTRY_CAP {
            output.push_str(&format!(
                "\n  ... and {} more items",
                entry_count - OUTPUT_ENTRY_CAP
            ));
        }
        Ok(json!({
            "success": true,
            "output": output,
            "metadata": {"entry_count": entry_count},
        }))
    }

    fn delete(&self, path: &Path) -> Result<Value, super::NativeToolError> {
        if !path.exists() {
            return Ok(fail(&format!("Path not found: {}", path.display())));
        }
        let outcome = if path.is_dir() {
            fs::remove_dir_all(path).map(|_| format!("Deleted directory: {}", path.display()))
        } else {
            fs::remove_file(path).map(|_| format!("Deleted file: {}", path.display()))
        };
        match outcome {
            Ok(output) => Ok(json!({"success": true, "output": output})),
            Err(e) => Ok(tool_execution_failure("delete", &e)),
        }
    }

    fn mkdir(&self, path: &Path) -> Result<Value, super::NativeToolError> {
        match fs::create_dir_all(path) {
            Ok(()) => Ok(json!({
                "success": true,
                "output": format!("Created directory: {}", path.display()),
            })),
            Err(e) => Ok(tool_execution_failure("mkdir", &e)),
        }
    }

    fn copy(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        let Some(dest_str) = args.get("destination").and_then(Value::as_str) else {
            return Ok(fail("destination is required for copy operation"));
        };
        let dest = match self.validate(dest_str) {
            Ok(dest) => dest,
            Err(message) => return Ok(fail(&message)),
        };
        let outcome = if path.is_dir() {
            // shutil.copytree: the destination must not exist.
            if dest.exists() {
                Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("File exists: {}", dest.display()),
                ))
            } else {
                copy_tree(path, &dest)
            }
        } else {
            // shutil.copy2: copying *into* an existing directory lands at
            // dest/<basename>, like the peer.
            let target = if dest.is_dir() {
                dest.join(path.file_name().unwrap_or_default())
            } else {
                dest.clone()
            };
            fs::copy(path, &target).map(|_| ())
        };
        match outcome {
            Ok(()) => Ok(json!({
                "success": true,
                "output": format!("Copied {} to {}", path.display(), dest.display()),
            })),
            Err(e) => Ok(tool_execution_failure("copy", &e)),
        }
    }

    fn move_op(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        let Some(dest_str) = args.get("destination").and_then(Value::as_str) else {
            return Ok(fail("destination is required for move operation"));
        };
        let dest = match self.validate(dest_str) {
            Ok(dest) => dest,
            Err(message) => return Ok(fail(&message)),
        };
        // shutil.move: a destination directory receives the entry inside
        // itself; rename first, fall back to copy+delete across devices.
        let target = if dest.is_dir() {
            dest.join(path.file_name().unwrap_or_default())
        } else {
            dest.clone()
        };
        let outcome = fs::rename(path, &target).or_else(|_| {
            if path.is_dir() {
                copy_tree(path, &target).and_then(|_| fs::remove_dir_all(path))
            } else {
                fs::copy(path, &target)
                    .map(|_| ())
                    .and_then(|_| fs::remove_file(path))
            }
        });
        match outcome {
            Ok(()) => Ok(json!({
                "success": true,
                "output": format!("Moved {} to {}", path.display(), dest.display()),
            })),
            Err(e) => Ok(tool_execution_failure("move", &e)),
        }
    }

    fn glob(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        let pattern = args.get("pattern").and_then(Value::as_str).unwrap_or("*");
        // `_glob` defaults recursion to True (unlike `list`).
        let recursive = args
            .get("recursive")
            .and_then(Value::as_bool)
            .unwrap_or(true);

        if !path.exists() {
            return Ok(fail(&format!("Path not found: {}", path.display())));
        }
        if !path.is_dir() {
            return Ok(fail(&format!("Path is not a directory: {}", path.display())));
        }

        let mut matches: Vec<PathBuf> = Vec::new();
        collect_glob_matches(path, pattern, recursive, &mut matches);
        // Sort by full-path string, exactly like the peer.
        matches.sort_by_key(|p| p.to_string_lossy().into_owned());

        let match_count = matches.len();
        let mut lines: Vec<String> = Vec::new();
        for m in matches.iter().take(OUTPUT_ENTRY_CAP) {
            let rel = m.strip_prefix(path).unwrap_or(m);
            let suffix = if m.is_dir() { "/" } else { "" };
            lines.push(format!("  {}{suffix}", rel.display()));
        }
        let mut output = format!(
            "Glob '{pattern}' in {} ({match_count} matches):\n",
            path.display()
        );
        output.push_str(&lines.join("\n"));
        if match_count > OUTPUT_ENTRY_CAP {
            output.push_str(&format!(
                "\n  ... and {} more matches",
                match_count - OUTPUT_ENTRY_CAP
            ));
        }
        Ok(json!({
            "success": true,
            "output": output,
            "metadata": {"match_count": match_count, "pattern": pattern},
        }))
    }

    fn diff(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        let diff_content = args
            .get("diff_content")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !path.exists() {
            return Ok(fail(&format!("File not found: {}", path.display())));
        }
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(e) => return Ok(tool_execution_failure("diff", &e)),
        };
        let (old_content, _) = read_with_detection(&raw);
        let old_lines = old_content.split_inclusive('\n').count();
        let new_lines = diff_content.split_inclusive('\n').count();
        if old_lines > DIFF_MAX_LINES || new_lines > DIFF_MAX_LINES {
            return Ok(fail(&format!(
                "Native diff refused: sides are limited to {DIFF_MAX_LINES} lines \
                 (requested {old_lines}/{new_lines})"
            )));
        }
        let diff = unified_diff(&old_content, diff_content, &path.to_string_lossy());
        if diff.is_empty() {
            return Ok(json!({"success": true, "output": "No differences found"}));
        }
        Ok(json!({
            "success": true,
            "output": diff,
            "metadata": {"has_changes": true},
        }))
    }

    fn patch(&self, args: &Value, path: &Path) -> Result<Value, super::NativeToolError> {
        let patch_text = args.get("patch").and_then(Value::as_str).unwrap_or("");
        if patch_text.is_empty() {
            return Ok(fail("patch is required for patch operation"));
        }
        if !path.exists() {
            return Ok(fail(&format!("File not found: {}", path.display())));
        }
        let raw = match fs::read(path) {
            Ok(raw) => raw,
            Err(e) => return Ok(tool_execution_failure("patch", &e)),
        };
        let (old_content, _) = read_with_detection(&raw);
        let (new_content, applied) = apply_patch(&old_content, patch_text);
        if let Err(e) = fs::write(path, new_content.as_bytes()) {
            return Ok(tool_execution_failure("patch", &e));
        }
        Ok(json!({
            "success": true,
            "output": format!("Applied {applied} hunk(s) to {}", path.display()),
        }))
    }
}

// ── Result shapes (the peer's ToolResult payload) ──────────────────────

/// A business failure in the peer's `ToolResult(success=False)` shape.
fn fail(error: &str) -> Value {
    json!({"success": false, "error": error})
}

/// The peer's `ToolExecutionError` path: an exception during an operation
/// becomes `Failed to <op>: <cause>` (the registry's business failure).
fn tool_execution_failure(op: &str, e: &std::io::Error) -> Value {
    fail(&format!("Failed to {op}: {e}"))
}

// ── Path helpers ───────────────────────────────────────────────────────

/// Lexically normalize a path (resolve `.` and `..` without touching the
/// filesystem) — the in-memory part of Python's `Path.resolve()`.
pub(crate) fn normalize_lexical(path: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if out.as_os_str().is_empty() {
                    // Relative climb past the start: keep the `..`
                    // (pathlib keeps it; the sandbox check then refuses
                    // any escape from the base).
                    out.push("..");
                } else if !out.pop() {
                    // Absolute path already at the filesystem root:
                    // pathlib clamps `/../etc` to `/etc` — `..` is a
                    // no-op there.
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// The sandbox root used by both engines: absolute, lexically normalized,
/// canonicalized when it exists (the Python tool resolves its `_base_dir`
/// at construction the same way).
pub(crate) fn resolve_base_dir(base_dir: PathBuf) -> PathBuf {
    let absolute = if base_dir.is_absolute() {
        base_dir
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(base_dir)
    };
    let normalized = normalize_lexical(&absolute);
    normalized.canonicalize().unwrap_or(normalized)
}

/// Recursive walk mirroring `os.walk` (top-down, symlink dirs not
/// followed). `visit` receives the root-relative path (dirs suffixed `/`,
/// mirroring the peer's rendering) and whether the entry is a directory;
/// entries are sorted per directory for determinism (the peer relies on
/// its own `sorted` calls at the same spots).
fn walk_sorted(root: &Path, visit: &mut dyn FnMut(&str, bool)) {
    let mut stack: Vec<PathBuf> = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        let mut children: Vec<PathBuf> = match fs::read_dir(&dir) {
            Ok(iter) => iter.filter_map(|e| e.ok()).map(|e| e.path()).collect(),
            Err(_) => continue, // unreadable dirs are skipped, like os.walk
        };
        children.sort();
        for child in children {
            let is_dir = child.is_dir() && !is_symlink(&child);
            let rel = child
                .strip_prefix(root)
                .unwrap_or(&child)
                .to_string_lossy()
                .into_owned();
            if is_dir {
                stack.push(child.clone());
                visit(&format!("{rel}/"), true);
            } else {
                visit(&rel, false);
            }
        }
    }
}

fn is_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_symlink())
        .unwrap_or(false)
}

/// Recursive directory copy (`shutil.copytree` shape; the caller checks
/// the must-not-exist rule).
fn copy_tree(src: &Path, dest: &Path) -> std::io::Result<()> {
    fs::create_dir_all(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let target = dest.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

/// Collect glob matches under `root`. `pattern` follows
/// `pathlib`/`fnmatch` semantics for the cases the peer uses: a bare name
/// pattern matches each entry's *name* at any depth when recursive
/// (`rglob`), within one level otherwise (`glob`); a pattern containing
/// `/` is matched against the root-relative path with `**` crossing
/// directories.
fn collect_glob_matches(root: &Path, pattern: &str, recursive: bool, out: &mut Vec<PathBuf>) {
    let mut all: Vec<PathBuf> = Vec::new();
    walk_sorted(root, &mut |rel, _| {
        all.push(root.join(rel.trim_end_matches('/')));
    });
    for candidate in all {
        let rel = candidate.strip_prefix(root).unwrap_or(&candidate);
        let name = candidate
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let matched = if pattern.contains('/') {
            glob_match_rel(rel, pattern)
        } else if recursive {
            // rglob("*.py") ≡ "**/*.py": match the entry name at any depth.
            fnmatch(&name, pattern)
        } else {
            // glob("*.py"): entries of the root directory only.
            rel.parent().is_none_or(|p| p.as_os_str().is_empty())
                && fnmatch(&name, pattern)
        };
        if matched {
            out.push(candidate);
        }
    }
}

/// Match a root-relative path against a pattern with `/` separators and
/// `**` (cross-directory), `*`/`?`/`[...]` within one segment.
fn glob_match_rel(path: &Path, pattern: &str) -> bool {
    let path_str = path.to_string_lossy().into_owned();
    let pattern_segments: Vec<&str> = pattern.split('/').collect();
    let path_segments: Vec<&str> = path_str.split('/').collect();
    match_segments(&path_segments, &pattern_segments)
}

fn match_segments(path: &[&str], pattern: &[&str]) -> bool {
    match pattern.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if *first == "**" => {
            // `**` matches zero or more segments.
            (0..=path.len()).any(|skip| match_segments(&path[skip..], rest))
        }
        Some((first, rest)) => {
            let Some((head, tail)) = path.split_first() else {
                return false;
            };
            fnmatch(head, first) && match_segments(tail, rest)
        }
    }
}

/// POSIX `fnmatch` (the peer's `_matches_glob`): `*`, `?`, and `[...]` /
/// `[!...]` classes, case-sensitive (POSIX `os.path.normcase` is identity).
pub fn fnmatch(name: &str, pattern: &str) -> bool {
    let name: Vec<char> = name.chars().collect();
    let pattern: Vec<char> = pattern.chars().collect();
    fnmatch_chars(&name, &pattern)
}

fn fnmatch_chars(name: &[char], pattern: &[char]) -> bool {
    match (pattern.first(), name.first()) {
        (None, None) => true,
        (None, Some(_)) => false,
        (Some('*'), _) => {
            // Collapse consecutive stars, then try consuming 0..n chars.
            let rest = &pattern[pattern.iter().take_while(|c| **c == '*').count()..];
            (0..=name.len()).any(|skip| fnmatch_chars(&name[skip..], rest))
        }
        (Some('?'), Some(_)) => fnmatch_chars(&name[1..], &pattern[1..]),
        (Some('['), Some(_)) => {
            let Some(close) = pattern[1..].iter().position(|c| *c == ']') else {
                // Unterminated class: literal '['.
                return name[0] == '[' && fnmatch_chars(&name[1..], &pattern[1..]);
            };
            let class = &pattern[1..1 + close];
            let (negated, class) = match class.first() {
                Some('!') => (true, &class[1..]),
                _ => (false, class),
            };
            let hit = class_matches(class, name[0]);
            if hit != negated {
                fnmatch_chars(&name[1..], &pattern[2 + close..])
            } else {
                false
            }
        }
        (Some(p), Some(c)) if p == c => fnmatch_chars(&name[1..], &pattern[1..]),
        _ => false,
    }
}

fn class_matches(class: &[char], c: char) -> bool {
    let mut hit = false;
    let mut idx = 0;
    while idx < class.len() {
        if idx + 2 < class.len() && class[idx + 1] == '-' {
            if c >= class[idx] && c <= class[idx + 2] {
                hit = true;
            }
            idx += 3;
        } else {
            if class[idx] == c {
                hit = true;
            }
            idx += 1;
        }
    }
    hit
}

// ── Encoding detection (`detect_encoding` / `_read_with_detection`) ────

/// Decode raw bytes the way `_read_with_detection` does: BOM-first, then
/// strict UTF-8, then the lossy last resort. Returns `(content, encoding)`.
pub fn read_with_detection(raw: &[u8]) -> (String, &'static str) {
    if raw.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(&raw[3..]).into_owned(), "utf-8-sig");
    }
    if raw.starts_with(&[0xFF, 0xFE]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect();
        return (String::from_utf16_lossy(&units), "utf-16-le");
    }
    if raw.starts_with(&[0xFE, 0xFF]) {
        let units: Vec<u16> = raw[2..]
            .chunks_exact(2)
            .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
            .collect();
        return (String::from_utf16_lossy(&units), "utf-16-be");
    }
    match std::str::from_utf8(raw) {
        Ok(content) => (content.to_owned(), "utf-8"),
        // The peer's own last-resort branch: `decode("utf-8", "replace")`
        // under the label `utf-8 (fallback)`.
        Err(_) => (
            String::from_utf8_lossy(raw).into_owned(),
            "utf-8 (fallback)",
        ),
    }
}

// ── Unified diff (`unified_diff`) ──────────────────────────────────────

/// A unified diff between `old` and `new`, labelled `a/{path}` / `b/{path}`
/// with `difflib`'s 3-line context and range formatting. Empty when the
/// texts are equal (mirroring `"".join(diff_lines) if diff_lines else ""`).
/// Lines are emitted verbatim (with their own `\n`), exactly like
/// `difflib.unified_diff`.
pub fn unified_diff(old: &str, new: &str, path: &str) -> String {
    let old_lines: Vec<&str> = old.split_inclusive('\n').collect();
    let new_lines: Vec<&str> = new.split_inclusive('\n').collect();
    let opcodes = lcs_opcodes(&old_lines, &new_lines);
    let groups = grouped_opcodes(&opcodes, 3);
    if groups.is_empty() {
        return String::new();
    }
    let mut out = format!("--- a/{path}\n+++ b/{path}\n");
    for group in &groups {
        let (i1, i2, j1, j2) = group_bounds(group);
        out.push_str(&format!(
            "@@ -{} +{} @@\n",
            format_range(i1, i2),
            format_range(j1, j2)
        ));
        for &(tag, a1, a2, b1, b2) in group {
            match tag {
                OpTag::Equal => {
                    for line in &old_lines[a1..a2] {
                        out.push(' ');
                        out.push_str(line);
                    }
                }
                OpTag::Delete => {
                    for line in &old_lines[a1..a2] {
                        out.push('-');
                        out.push_str(line);
                    }
                }
                OpTag::Insert => {
                    for line in &new_lines[b1..b2] {
                        out.push('+');
                        out.push_str(line);
                    }
                }
            }
        }
    }
    out
}

/// `difflib._format_range_unified`.
fn format_range(start: usize, stop: usize) -> String {
    let beginning = start + 1;
    let length = stop - start;
    if length == 1 {
        return format!("{beginning}");
    }
    let beginning = if length == 0 { beginning - 1 } else { beginning };
    format!("{beginning},{length}")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum OpTag {
    Equal,
    Delete,
    Insert,
}

type Opcode = (OpTag, usize, usize, usize, usize);

/// LCS-based opcodes (`get_opcodes` shape; a replace shows up as a
/// Delete run immediately followed by an Insert run — the same rendering
/// order `difflib` uses for replaces). Deterministic.
fn lcs_opcodes(a: &[&str], b: &[&str]) -> Vec<Opcode> {
    let n = a.len();
    let m = b.len();
    if n == 0 && m == 0 {
        return Vec::new();
    }
    // LCS length table ((n+1) × (m+1)), bounded by DIFF_MAX_LINES.
    let mut table = vec![0u32; (n + 1) * (m + 1)];
    let idx = |i: usize, j: usize| i * (m + 1) + j;
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            table[idx(i, j)] = if a[i] == b[j] {
                table[idx(i + 1, j + 1)] + 1
            } else {
                table[idx(i + 1, j)].max(table[idx(i, j + 1)])
            };
        }
    }
    let mut opcodes: Vec<Opcode> = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if a[i] == b[j] {
            push_opcode(&mut opcodes, OpTag::Equal, i, i + 1, j, j + 1);
            i += 1;
            j += 1;
        } else if table[idx(i + 1, j)] >= table[idx(i, j + 1)] {
            push_opcode(&mut opcodes, OpTag::Delete, i, i + 1, j, j);
            i += 1;
        } else {
            push_opcode(&mut opcodes, OpTag::Insert, i, i, j, j + 1);
            j += 1;
        }
    }
    while i < n {
        push_opcode(&mut opcodes, OpTag::Delete, i, i + 1, j, j);
        i += 1;
    }
    while j < m {
        push_opcode(&mut opcodes, OpTag::Insert, i, i, j, j + 1);
        j += 1;
    }
    opcodes
}

/// Append a unit opcode, coalescing with a previous contiguous same-kind
/// run (keeps the `get_opcodes` contiguous-runs shape).
fn push_opcode(opcodes: &mut Vec<Opcode>, tag: OpTag, i1: usize, i2: usize, j1: usize, j2: usize) {
    if let Some(last) = opcodes.last_mut() {
        if last.0 == tag && last.2 == i1 && last.4 == j1 {
            last.2 = i2;
            last.4 = j2;
            return;
        }
    }
    opcodes.push((tag, i1, i2, j1, j2));
}

/// `SequenceMatcher.get_grouped_opcodes(n)` — context-trimmed opcode
/// groups (3-line context, hunks split on equal runs longer than `2n`).
fn grouped_opcodes(codes: &[Opcode], n: usize) -> Vec<Vec<Opcode>> {
    if codes.is_empty() {
        return Vec::new();
    }
    let mut codes = codes.to_vec();
    if let Some(first) = codes.first_mut() {
        if first.0 == OpTag::Equal {
            let (tag, i1, i2, j1, j2) = *first;
            *first = (
                tag,
                i1.max(i2.saturating_sub(n)),
                i2,
                j1.max(j2.saturating_sub(n)),
                j2,
            );
        }
    }
    if let Some(last) = codes.last_mut() {
        if last.0 == OpTag::Equal {
            let (tag, i1, i2, j1, j2) = *last;
            *last = (tag, i1, i2.min(i1 + n), j1, j2.min(j1 + n));
        }
    }
    let nn = n * 2;
    let mut groups: Vec<Vec<Opcode>> = Vec::new();
    let mut group: Vec<Opcode> = Vec::new();
    for &(tag, i1, i2, j1, j2) in &codes {
        if tag == OpTag::Equal && i2 - i1 > nn {
            group.push((tag, i1, i1 + n, j1, j1 + n));
            groups.push(std::mem::take(&mut group));
            group.push((tag, i2 - n, i2, j2 - n, j2));
            continue;
        }
        group.push((tag, i1, i2, j1, j2));
    }
    let is_trivial_equal = group.len() == 1 && group[0].0 == OpTag::Equal;
    if !group.is_empty() && !is_trivial_equal {
        groups.push(group);
    }
    groups
}

fn group_bounds(group: &[Opcode]) -> (usize, usize, usize, usize) {
    (
        group.first().map(|c| c.1).unwrap_or(0),
        group.last().map(|c| c.2).unwrap_or(0),
        group.first().map(|c| c.3).unwrap_or(0),
        group.last().map(|c| c.4).unwrap_or(0),
    )
}

// ── Patch (`_patch`) ───────────────────────────────────────────────────

/// Apply a unified diff with REAL unified-diff hunk semantics: context
/// lines re-emit the consumed old line, `-` removes it, `+` emits the new
/// one, and the consumed old range (`old_start .. old_start + consumed`)
/// is spliced out. Hunk context is still not *verified* against the file
/// (the peer's looseness is kept — positions come from the `@@ -i[,n]`
/// header alone). Documented delta: the peer's slice replacement
/// (`new_lines[start:end] = add_lines`, context dropped) destroys content
/// whenever a hunk carries context lines — that destructiveness is a bug,
/// not a contract, so bh-rtd does not transcribe it. Returns
/// `(new_content, applied_hunks)`.
pub fn apply_patch(old_content: &str, patch_text: &str) -> (String, usize) {
    let old_lines: Vec<String> = old_content
        .split_inclusive('\n')
        .map(str::to_owned)
        .collect();
    let patch_lines: Vec<&str> = patch_text.split_inclusive('\n').collect();
    let mut new_lines = old_lines;
    let mut applied = 0usize;

    let mut line_idx = 0usize;
    while line_idx < patch_lines.len() {
        let line = patch_lines[line_idx];
        if let Some(hunk) = parse_hunk_header(line) {
            let old_start = hunk.old_start.saturating_sub(1); // 0-indexed

            let mut replacement: Vec<String> = Vec::new();
            let mut consumed = 0usize;
            line_idx += 1;
            while line_idx < patch_lines.len() {
                let pline = patch_lines[line_idx];
                if pline.starts_with("@@") || !pline.starts_with([' ', '-', '+']) {
                    break;
                }
                match pline.as_bytes()[0] {
                    b'+' => replacement.push(pline[1..].to_owned()),
                    b'-' => consumed += 1,
                    _ => {
                        // Context: the old line is authoritative (the file
                        // wins over a lossy diff render); a context line
                        // past the file end is emitted verbatim.
                        match new_lines.get(old_start + consumed) {
                            Some(old) => replacement.push(old.clone()),
                            None => {
                                replacement.push(pline[1..].to_owned());
                            }
                        }
                        consumed += 1;
                    }
                }
                line_idx += 1;
            }

            if old_start <= new_lines.len() {
                let end = (old_start + consumed).min(new_lines.len());
                new_lines.splice(old_start..end, replacement);
                applied += 1;
            }
            continue;
        }
        line_idx += 1;
    }
    (new_lines.concat(), applied)
}

struct HunkHeader {
    old_start: usize,
}

/// Parse `@@ -N[,M] +N[,M] @@` (the peer's `hunk_re`). The counts are
/// validated but not used for application: the hunk body is consumed
/// structurally (` `/`-`/`+` lines until the next header), which is what
/// makes the context-aware walk robust against count-less headers.
fn parse_hunk_header(line: &str) -> Option<HunkHeader> {
    let rest = line.strip_prefix("@@ -")?;
    let (old_part, plus_part) = rest.split_once(' ')?;
    let plus_part = plus_part.strip_prefix('+')?;
    let (new_part, _tail) = plus_part.split_once(" @@")?;
    let (old_start, _old_count) = split_count(old_part);
    let _ = split_count(new_part);
    Some(HunkHeader { old_start })
}

fn split_count(part: &str) -> (usize, usize) {
    match part.split_once(',') {
        Some((start, count)) => (
            start.trim().parse().unwrap_or(1),
            count.trim().parse().unwrap_or(1),
        ),
        None => (part.trim().parse().unwrap_or(1), 1),
    }
}

// ── Schemas (verbatim transcription of `FileTool.definition`) ──────────

/// The `file` input schema — the 1:1 JSON-Schema conversion of
/// `FileTool.definition` parameters.
pub fn input_schema() -> Value {
    json!({
        "type": "object",
        "required": ["operation", "path"],
        "properties": {
            "operation": {
                "type": "string",
                "description": "Operation: read, write, edit, list, delete, mkdir, copy, move, glob, diff, patch",
                "enum": ["read", "write", "edit", "list", "delete",
                         "mkdir", "copy", "move", "glob", "diff", "patch"],
            },
            "path": {"type": "string", "description": "File or directory path"},
            "content": {
                "type": "string",
                "description": "Content to write (for write/edit operations)",
            },
            "old_text": {
                "type": "string",
                "description": "Text to replace (for edit operation)",
            },
            "new_text": {
                "type": "string",
                "description": "Replacement text (for edit operation)",
            },
            "destination": {
                "type": "string",
                "description": "Destination path (for copy/move operations)",
            },
            "pattern": {
                "type": "string",
                "description": "Glob pattern to match (for glob operation)",
            },
            "recursive": {
                "type": "boolean",
                "description": "Recursive operation (for list/delete/mkdir/glob)",
                "default": false,
            },
            "replace_all": {
                "type": "boolean",
                "description": "Replace all occurrences in edit (default: first only)",
                "default": false,
            },
            "diff_content": {
                "type": "string",
                "description": "New content to diff against current file (for diff operation)",
            },
            "patch": {
                "type": "string",
                "description": "Unified diff to apply (for patch operation)",
            },
        },
    })
}

/// The `file` output schema — the `BaseTool` family's `ToolResult` payload.
pub fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "success": {"type": "boolean"},
            "output": {"type": "string"},
            "error": {"type": "string"},
            "metadata": {"type": "object"},
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
                "bh-rap-file-{tag}-{}-{:?}",
                std::process::id(),
                std::thread::current().id()
            ));
            fs::create_dir_all(&root).expect("mkdir sandbox");
            Self { root }
        }

        fn engine(&self) -> FileEngine {
            FileEngine::new(&self.root)
        }

        fn call(&self, args: Value) -> Value {
            self.engine().execute(&args).expect("file tool executes")
        }
    }

    impl Drop for Sandbox {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    #[test]
    fn write_read_edit_roundtrip() {
        let sandbox = Sandbox::new("wre");

        let result = sandbox.call(json!({
            "operation": "write", "path": "notes/a.txt", "content": "alpha beta\ngamma\n",
        }));
        assert_eq!(result["success"], true);
        assert_eq!(
            result["output"],
            format!(
                "Successfully wrote 17 characters to {}",
                sandbox.root.join("notes/a.txt").display()
            )
        );

        let result = sandbox.call(json!({"operation": "read", "path": "notes/a.txt"}));
        assert_eq!(result["success"], true);
        assert_eq!(result["output"], "alpha beta\ngamma\n");
        assert_eq!(result["metadata"]["encoding"], "utf-8");
        assert_eq!(result["metadata"]["size_bytes"], 17);

        let result = sandbox.call(json!({
            "operation": "edit", "path": "notes/a.txt",
            "old_text": "beta", "new_text": "BETA",
        }));
        assert_eq!(result["success"], true);
        assert!(result["output"]
            .as_str()
            .expect("output")
            .starts_with("Replaced 1 occurrence(s) of old_text in "));

        let result = sandbox.call(json!({
            "operation": "edit", "path": "notes/a.txt",
            "old_text": "gamma", "new_text": "GAMMA", "replace_all": true,
        }));
        assert_eq!(result["success"], true);
        let result = sandbox.call(json!({"operation": "read", "path": "notes/a.txt"}));
        assert_eq!(result["output"], "alpha BETA\nGAMMA\n");
    }

    #[test]
    fn sandbox_refuses_traversal_and_reports_missing_files() {
        let sandbox = Sandbox::new("trav");

        for escape in ["../outside.txt", "a/../../outside.txt", "/etc/hostname"] {
            let result = sandbox.call(json!({"operation": "write", "path": escape, "content": "x"}));
            assert_eq!(result["success"], false, "{escape}");
            assert_eq!(result["error"], format!("Path traversal detected: {escape}"));
        }
        // Traversal through a symlinked parent is refused too.
        let outside =
            std::env::temp_dir().join(format!("bh-rap-file-outside-{}", std::process::id()));
        fs::create_dir_all(&outside).expect("outside dir");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&outside, sandbox.root.join("link")).expect("symlink");
            let result = sandbox
                .call(json!({"operation": "write", "path": "link/escape.txt", "content": "x"}));
            assert_eq!(result["success"], false);
            assert_eq!(result["error"], "Path traversal detected: link/escape.txt");
        }
        let _ = fs::remove_dir_all(&outside);

        let result = sandbox.call(json!({"operation": "read", "path": "ghost.txt"}));
        assert_eq!(result["success"], false);
        assert_eq!(
            result["error"],
            format!("File not found: {}", sandbox.root.join("ghost.txt").display())
        );
    }

    #[test]
    fn list_glob_and_delete_mirror_the_peer_output() {
        let sandbox = Sandbox::new("listglob");
        for (path, content) in [
            ("src/main.rs", "fn main() {}\n"),
            ("src/lib.rs", "// lib\n"),
            ("docs/readme.md", "# doc\n"),
        ] {
            sandbox.call(json!({"operation": "write", "path": path, "content": content}));
        }

        // Non-recursive (the peer default): top-level names only.
        let result = sandbox.call(json!({"operation": "list", "path": "."}));
        assert_eq!(result["success"], true);
        let output = result["output"].as_str().expect("output");
        assert!(output.starts_with(&format!("Contents of {}:\n", sandbox.root.display())));
        assert!(output.contains("  docs/"));
        assert!(output.contains("  src/"));
        assert!(!output.contains("src/main.rs"), "non-recursive list is one level");
        assert_eq!(result["metadata"]["entry_count"], 2);

        // Recursive: the full walk with dirs suffixed `/`.
        let result = sandbox.call(json!({"operation": "list", "path": ".", "recursive": true}));
        assert_eq!(result["success"], true);
        let output = result["output"].as_str().expect("output");
        assert!(output.contains("  docs/"));
        assert!(output.contains("  src/"));
        assert!(output.contains("src/main.rs"));
        assert_eq!(result["metadata"]["entry_count"], 5);

        let result = sandbox.call(json!({"operation": "glob", "path": ".", "pattern": "*.rs"}));
        let output = result["output"].as_str().expect("glob output");
        assert!(output.starts_with("Glob '*.rs' in "));
        assert!(output.contains("(2 matches)"));
        assert!(output.contains("  src/lib.rs"));
        assert_eq!(result["metadata"]["match_count"], 2);

        let result = sandbox.call(json!({"operation": "glob", "path": ".", "pattern": "**/*.md"}));
        assert_eq!(result["metadata"]["match_count"], 1);

        let result = sandbox.call(json!({"operation": "delete", "path": "docs/readme.md"}));
        assert_eq!(result["success"], true);
        assert!(result["output"].as_str().expect("out").starts_with("Deleted file: "));
        let result = sandbox.call(json!({"operation": "delete", "path": "docs"}));
        assert!(result["output"]
            .as_str()
            .expect("out")
            .starts_with("Deleted directory: "));
    }

    #[test]
    fn mkdir_copy_move_behave_like_the_peer() {
        let sandbox = Sandbox::new("mkcpmv");
        sandbox.call(json!({"operation": "mkdir", "path": "deep/nested/dir"}));
        assert!(sandbox.root.join("deep/nested/dir").is_dir());

        sandbox.call(json!({"operation": "write", "path": "orig.txt", "content": "payload"}));
        let result = sandbox.call(json!({
            "operation": "copy", "path": "orig.txt", "destination": "deep/copy.txt",
        }));
        assert!(result["output"].as_str().expect("out").starts_with("Copied "));
        assert_eq!(
            fs::read_to_string(sandbox.root.join("deep/copy.txt")).expect("copy content"),
            "payload"
        );

        let result = sandbox.call(json!({
            "operation": "move", "path": "deep/copy.txt", "destination": "moved.txt",
        }));
        assert!(result["output"].as_str().expect("out").starts_with("Moved "));
        assert!(!sandbox.root.join("deep/copy.txt").exists());
        assert!(sandbox.root.join("moved.txt").exists());

        // Missing destination is the peer's business failure.
        let result = sandbox.call(json!({"operation": "copy", "path": "orig.txt"}));
        assert_eq!(result["error"], "destination is required for copy operation");
    }

    #[test]
    fn diff_renders_a_unified_diff_and_patch_applies_it() {
        let sandbox = Sandbox::new("diffpatch");
        sandbox.call(json!({
            "operation": "write", "path": "code.py",
            "content": "a\nb\nc\nd\ne\nf\ng\nh\ni\nj\n",
        }));

        let result = sandbox.call(json!({
            "operation": "diff", "path": "code.py",
            "diff_content": "a\nb\nc\nD\ne\nf\ng\nh\ni\nJ\n",
        }));
        assert_eq!(result["success"], true);
        let diff = result["output"].as_str().expect("diff");
        assert!(diff.starts_with("--- a/"));
        assert!(diff.contains("+++ b/"));
        assert!(diff.contains("-d\n+D\n"), "got: {diff}");
        assert!(diff.contains("-j\n+J\n"), "got: {diff}");
        assert!(diff.contains("@@ -"));

        // Apply it back: the patch operation reproduces the new content.
        let result = sandbox.call(json!({"operation": "patch", "path": "code.py", "patch": diff}));
        assert_eq!(
            result["output"],
            format!("Applied 1 hunk(s) to {}", sandbox.root.join("code.py").display())
        );
        let result = sandbox.call(json!({"operation": "read", "path": "code.py"}));
        assert_eq!(result["output"], "a\nb\nc\nD\ne\nf\ng\nh\ni\nJ\n");

        // Identical content → the peer's "No differences found".
        let result = sandbox.call(json!({
            "operation": "diff", "path": "code.py",
            "diff_content": "a\nb\nc\nD\ne\nf\ng\nh\ni\nJ\n",
        }));
        assert_eq!(result["output"], "No differences found");
    }

    #[test]
    fn read_detects_bom_encodings_like_the_peer() {
        let sandbox = Sandbox::new("enc");
        let mut bom: Vec<u8> = vec![0xEF, 0xBB, 0xBF];
        bom.extend_from_slice(b"hi\n");
        fs::write(sandbox.root.join("bom.txt"), bom).expect("bom file");
        let mut le: Vec<u8> = vec![0xFF, 0xFE];
        le.extend("hi\n".encode_utf16().flat_map(u16::to_le_bytes));
        fs::write(sandbox.root.join("le.txt"), le).expect("le file");

        let result = sandbox.call(json!({"operation": "read", "path": "bom.txt"}));
        assert_eq!(result["metadata"]["encoding"], "utf-8-sig");
        assert_eq!(result["output"], "hi\n");

        let result = sandbox.call(json!({"operation": "read", "path": "le.txt"}));
        assert_eq!(result["metadata"]["encoding"], "utf-16-le");
        assert_eq!(result["output"], "hi\n");
    }

    #[test]
    fn oversized_reads_and_unknown_operations_fail_loud() {
        let sandbox = Sandbox::new("limits");
        let result = sandbox.call(json!({"operation": "truncate", "path": "x"}));
        assert_eq!(result["error"], "Unknown operation: truncate");

        // > 50 MiB is refused before reading.
        let big = sandbox.root.join("big.bin");
        let file = fs::File::create(&big).expect("create");
        file.set_len(MAX_FILE_SIZE_BYTES + 1).expect("resize");
        let result = sandbox.call(json!({"operation": "read", "path": "big.bin"}));
        assert_eq!(
            result["error"],
            format!(
                "File too large: {} bytes (max: {MAX_FILE_SIZE_BYTES})",
                MAX_FILE_SIZE_BYTES + 1
            )
        );

        // Missing path is the peer's own business failure.
        let result = sandbox.call(json!({"operation": "read"}));
        assert_eq!(result["error"], "No path specified");
    }

    #[test]
    fn fnmatch_covers_the_peer_use_cases() {
        assert!(fnmatch("main.rs", "*.rs"));
        // `?` matches exactly one character: "main.?s" fits "main.rs",
        // "main.??s" needs eight characters and cannot.
        assert!(fnmatch("main.rs", "main.?s"));
        assert!(!fnmatch("main.rs", "main.??s"));
        assert!(!fnmatch("main.rs", "*.py"));
        assert!(fnmatch("test_a.py", "test_[abc].py"));
        assert!(!fnmatch("test_b.py", "test_[!ab].py"));
        assert!(fnmatch("anything.log", "*"));
    }

    #[test]
    fn normalize_lexical_resolves_dots() {
        let base = PathBuf::from("/ws");
        assert_eq!(
            normalize_lexical(&base.join("a/../b")),
            PathBuf::from("/ws/b")
        );
        assert_eq!(
            normalize_lexical(&base.join("../../etc")),
            PathBuf::from("/etc")
        );
    }
}
