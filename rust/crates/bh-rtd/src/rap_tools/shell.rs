//! Native `shell.execute` — the Rust mirror of
//! `src/agent_tools/shell.py#ShellTool` (PLAN.md §4 Phase 4).
//!
//! Mirrored semantics (verbatim from the Python source):
//!
//! * **Blocked commands** ([`BLOCKED_COMMANDS`]) — case-insensitive
//!   substring check on the trimmed command, identical pattern set; a hit
//!   returns the peer's own failure shape (`exit_code: -2`, stderr
//!   `Command blocked by safety policy: matches blocked pattern '...'`),
//!   never an execution. This is the Python-side allow/deny policy — it is
//!   mirrored 1:1, never loosened.
//! * **Working directory** must exist (`exit_code: -3` business failure
//!   `Working directory does not exist: ...`).
//! * **Shell binary** (default `/bin/bash`) must be an existing file
//!   (`exit_code: -4` `Shell binary not found: ...`).
//! * **Timeout** (default 30s): the process is killed at the deadline and
//!   the result is the peer's timeout shape (`exit_code: -1`,
//!   `timed_out: true`, `Command timed out after Ns`) — loud, never a
//!   hang and never a fake completion.
//! * **Environment**: the daemon's environment overlaid with `args.env`
//!   (string→string), like `os.environ.copy()` + `update`.
//! * **Output capture**: `capture_stdout` / `capture_stderr` (default
//!   `true`); a disabled stream is discarded, mirroring `DEVNULL`.
//! * **Result shape**: `{stdout, stderr, exit_code, timed_out, duration_ms,
//!   command, working_dir}` with `duration_ms` rounded to 2 decimals and
//!   `working_dir` defaulting to the daemon's working directory — the
//!   Python tool's exact dict.
//!
//! The read-only allow-list heuristic ([`SAFE_COMMAND_PREFIXES`] /
//! [`ShellEngine::is_safe_command`]) is mirrored too: the Python tool uses
//! it for autonomy classification, not for gating `execute`, so the native
//! mirror exposes it identically instead of inventing a stricter gate.
//!
//! Documented delta: the process runs `/bin/bash -c <command>` through
//! `std::process::Command` (the Python tool's
//! `asyncio.create_subprocess_shell(..., executable=shell_bin)`), and a
//! spawn failure surfaces as the peer's generic failure shape
//! (`exit_code: -1`, stderr = the OS error).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

use super::round2;

/// Commands that are considered destructive / irreversible — the exact
/// pattern set of `src/agent_tools/shell.py#_BLOCKED_COMMANDS`.
pub const BLOCKED_COMMANDS: [&str; 8] = [
    "rm -rf /",
    "mkfs",
    "dd if=",
    ":(){ :|:& };:",
    "shutdown",
    "reboot",
    "halt",
    "poweroff",
];

/// Commands that are read-only and safe at L1 — the exact prefix tuple of
/// `src/agent_tools/shell.py#_SAFE_COMMAND_PREFIXES` (the Python tool's
/// autonomy-classification heuristic, exposed for parity).
pub const SAFE_COMMAND_PREFIXES: [&str; 40] = [
    "ls",
    "cat",
    "head",
    "tail",
    "pwd",
    "echo",
    "which",
    "whoami",
    "env",
    "printenv",
    "id",
    "uname",
    "hostname",
    "date",
    "df",
    "du",
    "ps",
    "top",
    "free",
    "uptime",
    "wc",
    "grep",
    "find",
    "stat",
    "file",
    "diff",
    "sort",
    "uniq",
    "cut",
    "tr",
    "awk",
    "sed -n",
    "git status",
    "git log",
    "git diff",
    "git branch",
    "git remote",
    "python -c",
    "python3 -c",
    "node -e",
];

/// Default execution timeout in seconds (`input_schema` default).
pub const DEFAULT_TIMEOUT_SECS: u64 = 30;

/// The native `shell.execute` engine (stateless; mirrors `ShellTool`).
#[derive(Clone, Copy, Debug, Default)]
pub struct ShellEngine;

impl ShellEngine {
    /// Execute one `shell.execute` tool call (`args` is the tool's JSON
    /// object; see the module docs for the mirrored contract).
    ///
    /// # Errors
    ///
    /// [`NativeToolError::InvalidParams`] when the required `command`
    /// string is missing — the JSON-Schema contract, enforced loudly.
    pub fn execute(&self, args: &Value) -> Result<Value, super::NativeToolError> {
        let Some(command) = args.get("command").and_then(Value::as_str) else {
            return Err(super::NativeToolError::InvalidParams(
                "shell.execute requires string 'command'".into(),
            ));
        };
        let timeout = args
            .get("timeout")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_TIMEOUT_SECS);
        let working_dir_param = args.get("working_dir").and_then(Value::as_str);
        let shell_bin = args
            .get("shell")
            .and_then(Value::as_str)
            .unwrap_or("/bin/bash");
        let capture_stdout = args
            .get("capture_stdout")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let capture_stderr = args
            .get("capture_stderr")
            .and_then(Value::as_bool)
            .unwrap_or(true);
        let extra_env: Vec<(String, String)> = args
            .get("env")
            .and_then(Value::as_object)
            .map(|map| {
                map.iter()
                    .filter_map(|(k, v)| {
                        v.as_str().map(|s| (k.clone(), s.to_owned()))
                    })
                    .collect()
            })
            .unwrap_or_default();

        let start = Instant::now();
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let working_dir_default = working_dir_param
            .map(PathBuf::from)
            .unwrap_or_else(|| cwd.clone());

        // ── Safety check (mirrored `_check_blocked`) ──────────────────
        if let Some(blocked) = check_blocked(command) {
            return Ok(failure(
                "",
                &format!("Command blocked by safety policy: matches blocked pattern '{blocked}'"),
                -2,
                false,
                start,
                command,
                &working_dir_default,
            ));
        }

        // ── Working directory validation ──────────────────────────────
        if let Some(dir) = working_dir_param {
            if !Path::new(dir).is_dir() {
                return Ok(failure(
                    "",
                    &format!("Working directory does not exist: {dir}"),
                    -3,
                    false,
                    start,
                    command,
                    &PathBuf::from(dir),
                ));
            }
        }

        // ── Shell binary validation ───────────────────────────────────
        if !Path::new(shell_bin).is_file() {
            return Ok(failure(
                "",
                &format!("Shell binary not found: {shell_bin}"),
                -4,
                false,
                start,
                command,
                &working_dir_default,
            ));
        }

        // ── Environment (os.environ.copy() + update) ─────────────────
        let mut cmd = Command::new(shell_bin);
        cmd.arg("-c").arg(command);
        cmd.current_dir(
            working_dir_param
                .map(PathBuf::from)
                .unwrap_or_else(|| cwd.clone()),
        );
        cmd.stdout(if capture_stdout {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        cmd.stderr(if capture_stderr {
            Stdio::piped()
        } else {
            Stdio::null()
        });
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => {
                // Python catches the spawn failure generically and reports
                // it in the same result shape.
                return Ok(failure(
                    "",
                    &e.to_string(),
                    -1,
                    false,
                    start,
                    command,
                    &working_dir_default,
                ));
            }
        };

        // Drain the captured pipes concurrently (Python's communicate()
        // does the same): a chatty child must never deadlock on a full
        // pipe while we wait for the timeout deadline.
        let stdout_handle = child.stdout.take().and_then(drain_to_string);
        let stderr_handle = child.stderr.take().and_then(drain_to_string);

        let deadline = Instant::now() + Duration::from_secs(timeout.max(1));
        let mut timed_out = false;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break Ok(Some(status)),
                Ok(None) => {
                    if Instant::now() >= deadline {
                        timed_out = true;
                        let _ = child.kill();
                        break child.wait().map(Some);
                    }
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => break Err(e),
            }
        };

        let stdout_text = stdout_handle
            .and_then(|h| h.join().unwrap_or_default())
            .unwrap_or_default();
        let stderr_text = stderr_handle
            .and_then(|h| h.join().unwrap_or_default())
            .unwrap_or_default();

        match status {
            Ok(Some(status)) if !timed_out => {
                let exit_code = status.code().unwrap_or(-1);
                Ok(json!({
                    "stdout": stdout_text,
                    "stderr": stderr_text,
                    "exit_code": exit_code,
                    "timed_out": false,
                    "duration_ms": round2(elapsed_ms(start)),
                    "command": command,
                    "working_dir": working_dir_default.to_string_lossy(),
                }))
            }
            Ok(_) => {
                // Timeout (or the post-kill reap): the peer discards any
                // captured output and reports the timeout shape.
                Ok(failure(
                    "",
                    &format!("Command timed out after {timeout}s"),
                    -1,
                    true,
                    start,
                    command,
                    &working_dir_default,
                ))
            }
            Err(e) => Ok(failure(
                "",
                &e.to_string(),
                -1,
                false,
                start,
                command,
                &working_dir_default,
            )),
        }
    }

    /// Heuristic: true when the command looks read-only / safe at L1 — the
    /// exact mirror of `ShellTool.is_safe_command` over
    /// [`SAFE_COMMAND_PREFIXES`]. Classification only: the Python tool does
    /// not gate `execute` on it, and neither does this engine.
    pub fn is_safe_command(&self, command: &str) -> bool {
        let cmd_stripped = command.trim().to_ascii_lowercase();
        SAFE_COMMAND_PREFIXES
            .iter()
            .any(|prefix| cmd_stripped.starts_with(prefix))
    }
}

/// Return the blocked pattern that `command` contains (mirrored
/// `ShellTool._check_blocked`: case-insensitive substring on the trimmed
/// command), or `None` when the command is allowed.
pub fn check_blocked(command: &str) -> Option<&'static str> {
    let cmd_lower = command.trim().to_ascii_lowercase();
    BLOCKED_COMMANDS
        .iter()
        .find(|blocked| cmd_lower.contains(*blocked))
        .copied()
}

/// The Python tool's failure result shape (empty stdout, explanatory
/// stderr, negative exit code).
#[allow(clippy::too_many_arguments)]
fn failure(
    stdout: &str,
    stderr: &str,
    exit_code: i32,
    timed_out: bool,
    start: Instant,
    command: &str,
    working_dir: &Path,
) -> Value {
    json!({
        "stdout": stdout,
        "stderr": stderr,
        "exit_code": exit_code,
        "timed_out": timed_out,
        "duration_ms": round2(elapsed_ms(start)),
        "command": command,
        "working_dir": working_dir.to_string_lossy(),
    })
}

fn elapsed_ms(start: Instant) -> f64 {
    start.elapsed().as_secs_f64() * 1000.0
}

/// Drain one piped child stream to its end on a helper thread (lossy
/// UTF-8, mirroring Python's `decode(errors="replace")`).
fn drain_to_string<R: Read + Send + 'static>(
    mut stream: R,
) -> Option<std::thread::JoinHandle<Option<String>>> {
    std::thread::Builder::new()
        .name("bh-rtd-shell-drain".into())
        .spawn(move || {
            let mut buf = Vec::new();
            // A read failure mid-stream degrades to the partial output;
            // Python's decode-replace contract does not fabricate here.
            let _ = stream.read_to_end(&mut buf);
            Some(String::from_utf8_lossy(&buf).into_owned())
        })
        .ok()
}

/// The `shell.execute` input schema — the verbatim transcription of
/// `ShellTool.input_schema()` (kept next to the engine so the spec and the
/// engine cannot drift apart silently).
pub fn input_schema() -> Value {
    json!({
        "type": "object",
        "required": ["command"],
        "properties": {
            "command": {"type": "string", "description": "Shell command to execute"},
            "timeout": {
                "type": "integer", "default": 30,
                "description": "Execution timeout in seconds",
            },
            "working_dir": {
                "type": "string",
                "description": "Working directory for command execution",
            },
            "env": {
                "type": "object",
                "description": "Additional environment variables",
                "additionalProperties": {"type": "string"},
            },
            "shell": {
                "type": "string", "default": "/bin/bash",
                "description": "Shell binary to use",
            },
            "capture_stdout": {
                "type": "boolean", "default": true,
                "description": "Whether to capture stdout",
            },
            "capture_stderr": {
                "type": "boolean", "default": true,
                "description": "Whether to capture stderr",
            },
        },
    })
}

/// The `shell.execute` output schema — the verbatim transcription of
/// `ShellTool.output_schema()`.
pub fn output_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "stdout": {"type": "string"},
            "stderr": {"type": "string"},
            "exit_code": {"type": "integer"},
            "timed_out": {"type": "boolean"},
            "duration_ms": {"type": "number"},
            "command": {"type": "string"},
            "working_dir": {"type": "string"},
        },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn run(args: Value) -> Value {
        ShellEngine.execute(&args).expect("shell tool executes")
    }

    #[test]
    fn executes_a_command_and_captures_output() {
        let result = run(json!({"command": "printf 'hello-native'"}));
        assert_eq!(result["stdout"], "hello-native");
        assert_eq!(result["exit_code"], 0);
        assert_eq!(result["timed_out"], false);
        assert_eq!(result["command"], "printf 'hello-native'");
        assert!(result["working_dir"].is_string());
        assert!(result["duration_ms"].as_f64().expect("duration") >= 0.0);
    }

    #[test]
    fn non_zero_exit_codes_travel_in_the_result() {
        let result = run(json!({"command": "sh -c 'echo boom >&2; exit 7'"}));
        assert_eq!(result["exit_code"], 7);
        assert_eq!(result["stderr"], "boom\n");
        assert_eq!(result["timed_out"], false);
    }

    #[test]
    fn blocked_commands_mirror_the_python_policy() {
        for command in ["rm -rf /", "MKFS /dev/sda", "echo; shutdown -h now"] {
            let result = run(json!({ "command": command }));
            assert_eq!(result["exit_code"], -2, "{command}");
            assert_eq!(result["stdout"], "");
            let stderr = result["stderr"].as_str().expect("stderr");
            assert!(
                stderr.starts_with("Command blocked by safety policy: matches blocked pattern"),
                "got: {stderr}"
            );
        }
        // The standalone checker agrees with the engine.
        assert_eq!(check_blocked("sudo REBOOT now"), Some("reboot"));
        assert_eq!(check_blocked("ls -la"), None);
    }

    #[test]
    fn safe_command_prefixes_mirror_the_python_allow_list() {
        let engine = ShellEngine;
        assert!(engine.is_safe_command("ls -la"));
        assert!(engine.is_safe_command("  GIT STATUS"));
        assert!(engine.is_safe_command("sed -n '1p' file"));
        assert!(engine.is_safe_command("git branch -a"));
        assert!(engine.is_safe_command("python3 -c 'print(1)'"));
        assert!(!engine.is_safe_command("curl http://example.com"));
        assert!(!engine.is_safe_command("rm file"));
    }

    #[test]
    fn missing_working_dir_and_shell_binary_fail_like_the_peer() {
        let result = run(json!({
            "command": "ls",
            "working_dir": "/definitely/not/a/dir",
        }));
        assert_eq!(result["exit_code"], -3);
        assert_eq!(
            result["stderr"],
            "Working directory does not exist: /definitely/not/a/dir"
        );

        let result = run(json!({
            "command": "ls",
            "shell": "/no/such/shell",
        }));
        assert_eq!(result["exit_code"], -4);
        assert_eq!(result["stderr"], "Shell binary not found: /no/such/shell");
    }

    #[test]
    fn missing_command_is_a_typed_invalid_params_error() {
        let err = ShellEngine.execute(&json!({})).unwrap_err();
        assert!(matches!(err, super::super::NativeToolError::InvalidParams(_)));
    }

    #[test]
    fn timeout_kills_the_child_and_reports_the_peer_shape() {
        let result = run(json!({
            "command": "sleep 10",
            "timeout": 1,
        }));
        assert_eq!(result["timed_out"], true);
        assert_eq!(result["exit_code"], -1);
        assert_eq!(result["stdout"], "");
        assert_eq!(result["stderr"], "Command timed out after 1s");
    }

    #[test]
    fn extra_env_is_overlaid_and_capture_flags_are_honoured() {
        let result = run(json!({
            "command": "printf '%s' \"$BH_TEST_VAR\"",
            "env": {"BH_TEST_VAR": "env-mirror"},
        }));
        assert_eq!(result["stdout"], "env-mirror");

        let result = run(json!({
            "command": "echo out; echo err >&2",
            "capture_stdout": false,
            "capture_stderr": false,
        }));
        assert_eq!(result["stdout"], "");
        assert_eq!(result["stderr"], "");
        assert_eq!(result["exit_code"], 0);
    }

    #[test]
    fn env_inherits_the_daemon_environment() {
        // The mirror of `os.environ.copy()`: a var of this process reaches
        // the child. (`PATH` always exists in a spawning environment.)
        let result = run(json!({"command": "printf '%s' \"$PATH\""}));
        assert_eq!(
            result["stdout"],
            std::env::var("PATH").unwrap_or_default()
        );
    }
}
