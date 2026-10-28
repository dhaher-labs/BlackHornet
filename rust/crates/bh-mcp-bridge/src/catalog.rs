//! The BH tool catalog — the Rust-side mirror of the BlackHornet tool
//! registry semantics (`src/agent_tools/`).
//!
//! The Phase 1 rule is "bridges before ports": no tool logic lives here.
//! What the Rust side needs is the *catalog* — names, descriptions, JSON
//! schemas, autonomy levels — so it can
//!
//! 1. hand tool definitions to an Infinity agent (see
//!    [`crate::tool_bridge`] under the `infinity` feature), and
//! 2. drift-check whatever the Python peer's `tools/list` manifest claims
//!    against what this bridge was built from ([`validate_peer_manifest`]).
//!
//! Every entry is transcribed from the Python source and carries the exact
//! provenance in [`CatalogEntry::source`]. The transcription is mechanical:
//!
//! * `MCPTool` subclasses (the `src/agent_tools/base.py` family) declare
//!   `name()` / `category()` / `autonomy_level()` / `input_schema()` /
//!   `output_schema()` directly; their docstrings become the description.
//! * `FileTool` belongs to the older `src/core/tool_base.BaseTool` family:
//!   it declares a `ToolDefinition` with a parameter list instead. That list
//!   is converted 1:1 into a JSON Schema below (name/type/description/enum/
//!   default/required). `BaseTool` has no autonomy levels (it carries
//!   `requires_permission` instead), so `file` is declared L1 here — the
//!   read-only-conservative default — while the Python side keeps enforcing
//!   its own permissions at call time. The entry documents this caveat
//!   instead of silently inventing parity.
//!
//! Categories and autonomy semantics come from `src/agent_tools/base.py`:
//! L0 read-only, L1 safe operations, L2 moderate side-effects, L3 sensitive,
//! L4 destructive.

use std::sync::LazyLock;

use serde_json::{json, Value};

use crate::ToolManifest;

/// One statically mirrored BH tool.
#[derive(Clone, Debug, PartialEq)]
pub struct CatalogEntry {
    /// Dotted tool identifier, exactly as the Python tool's `name()`.
    pub name: &'static str,
    /// Tool category used for discovery grouping, as `category()`.
    pub category: &'static str,
    /// Human-readable description (the Python tool's docstring / definition
    /// description).
    pub description: &'static str,
    /// Minimum autonomy level (0-4), as `autonomy_level()`.
    pub autonomy_level: u8,
    /// JSON Schema of accepted args, as `input_schema()`.
    pub input_schema: Value,
    /// JSON Schema of the returned payload, as `output_schema()`.
    pub output_schema: Value,
    /// Python module + class this entry was transcribed from.
    pub source: &'static str,
}

impl CatalogEntry {
    /// The manifest shape the UDS peer serves for this tool (the subset of
    /// `MCPTool.describe()` this bridge carries).
    pub fn to_manifest(&self) -> ToolManifest {
        ToolManifest {
            name: self.name.to_owned(),
            category: self.category.to_owned(),
            autonomy_level: self.autonomy_level,
            input_schema: self.input_schema.clone(),
            output_schema: self.output_schema.clone(),
        }
    }
}

/// `file` — `src/agent_tools/file_tool.py#FileTool` (`BaseTool` family).
///
/// `input_schema` is the 1:1 JSON-Schema conversion of `FileTool.definition`
/// parameters; `output_schema` mirrors the `BaseTool` family's `ToolResult`
/// payload (`success` / `output` / `error` / `metadata`). See the module
/// docs for the autonomy-level caveat.
pub fn file() -> CatalogEntry {
    CatalogEntry {
        name: "file",
        category: "file",
        description: "Read, write, edit, list, search (glob), diff/patch files \
                      with safety controls and encoding detection",
        autonomy_level: 1,
        input_schema: json!({
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
    }),
    output_schema: json!({
        "type": "object",
        "properties": {
            "success": {"type": "boolean"},
            "output": {"type": "string"},
            "error": {"type": "string"},
            "metadata": {"type": "object"},
        },
        }),
        source: "src/agent_tools/file_tool.py#FileTool",
    }
}

/// `shell.execute` — `src/agent_tools/shell.py#ShellTool` (`MCPTool` family,
/// schemas transcribed verbatim).
pub fn shell_execute() -> CatalogEntry {
    CatalogEntry {
    name: "shell.execute",
    category: "compute",
    description: "Execute shell commands with timeout, output capture, \
                  working-directory control, and environment-variable support",
    autonomy_level: 1,
    input_schema: json!({
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
    }),
    output_schema: json!({
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
        }),
        source: "src/agent_tools/shell.py#ShellTool",
    }
}

/// `code.operations` — `src/agent_tools/code.py#CodeTool` (`MCPTool` family,
/// schemas transcribed verbatim).
pub fn code_operations() -> CatalogEntry {
    CatalogEntry {
    name: "code.operations",
    category: "compute",
    description: "Code analysis, linting, formatting, and sandboxed execution",
    autonomy_level: 1,
    input_schema: json!({
        "type": "object",
        "required": ["action", "code"],
        "properties": {
            "action": {
                "type": "string",
                "enum": ["syntax_check", "lint", "format", "analyze", "run"],
                "description": "Code action to perform",
            },
            "code": {"type": "string", "description": "Source code to process"},
            "language": {
                "type": "string", "default": "python",
                "enum": ["python", "javascript", "typescript", "go", "rust"],
                "description": "Programming language",
            },
            "timeout": {
                "type": "integer", "default": 10,
                "description": "Execution timeout in seconds (for 'run')",
            },
            "filename": {
                "type": "string",
                "description": "Optional filename for context",
            },
            "linter": {
                "type": "string", "default": "auto",
                "description": "Linter to use (auto, flake8, pylint, eslint)",
            },
            "formatter": {
                "type": "string", "default": "auto",
                "description": "Formatter to use (auto, black, autopep8, prettier)",
            },
        },
    }),
    output_schema: json!({
        "type": "object",
        "properties": {
            "success": {"type": "boolean"},
            "action": {"type": "string"},
            "result": {"type": "object"},
        },
        }),
        source: "src/agent_tools/code.py#CodeTool",
    }
}

/// The BH tool catalog mirrored on the Rust side (deterministic order).
static CATALOG: LazyLock<Vec<CatalogEntry>> =
    LazyLock::new(|| vec![file(), shell_execute(), code_operations()]);

/// The BH tool catalog mirrored on the Rust side (deterministic order).
pub fn bh_tool_catalog() -> &'static [CatalogEntry] {
    &CATALOG
}

/// Look up one catalog entry by dotted tool name.
pub fn catalog_entry(name: &str) -> Option<&'static CatalogEntry> {
    bh_tool_catalog().iter().find(|e| e.name == name)
}

/// Why a Python peer manifest disagrees with this crate's catalog.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogDrift {
    /// The peer does not list a tool this catalog mirrors.
    MissingTool { name: String },
    /// The peer lists the tool with a different schema, category, or
    /// autonomy level.
    ChangedTool {
        name: String,
        /// Human-readable list of the fields that disagree.
        differences: Vec<String>,
    },
}

impl core::fmt::Display for CatalogDrift {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            CatalogDrift::MissingTool { name } => {
                write!(f, "peer manifest is missing catalog tool {name:?}")
            }
            CatalogDrift::ChangedTool { name, differences } => {
                write!(f, "peer manifest tool {name:?} drifted: {}", differences.join("; "))
            }
        }
    }
}

impl std::error::Error for CatalogDrift {}

/// Compare a Python peer's `tools/list` manifest against the static catalog.
///
/// Drift here means the Python registry changed semantics the Rust side
/// mirrors (schemas, categories, autonomy, or the tool set itself); the
/// bridge reports it rather than serving a stale mirror. Extra peer tools
/// are *not* drift (the Python registry legitimately grows).
pub fn validate_peer_manifest(manifest: &[ToolManifest]) -> Result<(), Vec<CatalogDrift>> {
    let mut problems = Vec::new();
    for entry in bh_tool_catalog() {
        match manifest.iter().find(|m| m.name == entry.name) {
            None => problems.push(CatalogDrift::MissingTool {
                name: entry.name.to_owned(),
            }),
            Some(peer) => {
                let mut differences = Vec::new();
                if peer.category != entry.category {
                    differences.push(format!(
                        "category {:?} != catalog {:?}",
                        peer.category, entry.category
                    ));
                }
                if peer.autonomy_level != entry.autonomy_level {
                    differences.push(format!(
                        "autonomy_level {} != catalog {}",
                        peer.autonomy_level, entry.autonomy_level
                    ));
                }
                if peer.input_schema != entry.input_schema {
                    differences.push("input_schema differs".to_owned());
                }
                if peer.output_schema != entry.output_schema {
                    differences.push("output_schema differs".to_owned());
                }
                if !differences.is_empty() {
                    problems.push(CatalogDrift::ChangedTool {
                        name: entry.name.to_owned(),
                        differences,
                    });
                }
            }
        }
    }
    if problems.is_empty() {
        Ok(())
    } else {
        Err(problems)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn catalog_entries_have_canonical_names_and_objects() {
        let catalog = bh_tool_catalog();
        assert_eq!(catalog.len(), 3);
        assert_eq!(catalog[0].name, "file");
        assert_eq!(catalog[1].name, "shell.execute");
        assert_eq!(catalog[2].name, "code.operations");
        for entry in catalog {
            assert!(
                entry.input_schema.is_object() && entry.output_schema.is_object(),
                "{name}: schemas must be JSON objects",
                name = entry.name
            );
            assert!(entry.autonomy_level <= 4, "autonomy levels are 0-4");
            assert!(!entry.source.is_empty());
        }
        assert_eq!(catalog_entry("file").unwrap().category, "file");
        assert_eq!(catalog_entry("shell.execute").unwrap().category, "compute");
        assert!(catalog_entry("nope").is_none());
    }

    #[test]
    fn shell_schema_matches_the_python_source_contract() {
        // Spot-checks the transcribed ShellTool.input_schema(): required
        // command, defaults, and the env additionalProperties constraint.
        let schema = &shell_execute().input_schema;
        assert_eq!(schema["type"], "object");
        assert_eq!(schema["required"], json!(["command"]));
        assert_eq!(schema["properties"]["timeout"]["default"], 30);
        assert_eq!(schema["properties"]["shell"]["default"], "/bin/bash");
        assert_eq!(
            schema["properties"]["env"]["additionalProperties"],
            json!({"type": "string"})
        );
        assert_eq!(
            code_operations().input_schema["properties"]["action"]["enum"],
            json!(["syntax_check", "lint", "format", "analyze", "run"])
        );
        assert_eq!(
            code_operations().input_schema["properties"]["language"]["enum"],
            json!(["python", "javascript", "typescript", "go", "rust"])
        );
        // FileTool conversion: enum + required preserved from the
        // ToolParameter list.
        assert_eq!(
            file().input_schema["required"],
            json!(["operation", "path"])
        );
        assert_eq!(
            file().input_schema["properties"]["operation"]["enum"][0],
            json!("read")
        );
        assert_eq!(
            file().input_schema["properties"]["replace_all"]["default"],
            json!(false)
        );
    }

    #[test]
    fn manifests_round_trip_through_the_catalog() {
        let manifests: Vec<ToolManifest> =
            bh_tool_catalog().iter().map(|e| e.to_manifest()).collect();
        assert_eq!(manifests.len(), 3);
        assert_eq!(manifests[0].autonomy_level, 1);
        assert_eq!(manifests[1].name, "shell.execute");
        // The catalog's own manifest validates clean.
        assert!(validate_peer_manifest(&manifests).is_ok());
        // Extra peer tools are not drift.
        let mut with_extra = manifests.clone();
        with_extra.push(ToolManifest {
            name: "memory.manage".into(),
            category: "memory".into(),
            autonomy_level: 1,
            input_schema: json!({"type": "object"}),
            output_schema: json!({"type": "object"}),
        });
        assert!(validate_peer_manifest(&with_extra).is_ok());
    }

    #[test]
    fn drift_is_reported_not_masked() {
        let mut manifests: Vec<ToolManifest> =
            bh_tool_catalog().iter().map(|e| e.to_manifest()).collect();
        // Drop `file` entirely and mutate shell.execute's schema.
        manifests.remove(0);
        manifests[0].input_schema = json!({"type": "object", "required": ["cmd"]});
        let err = validate_peer_manifest(&manifests).expect_err("drift must be reported");
        assert_eq!(err.len(), 2);
        assert!(matches!(&err[0], CatalogDrift::MissingTool { name } if name == "file"));
        match &err[1] {
            CatalogDrift::ChangedTool { name, differences } => {
                assert_eq!(name, "shell.execute");
                assert!(differences.iter().any(|d| d.contains("input_schema")));
            }
            other => panic!("expected ChangedTool, got {other:?}"),
        }
        assert!(err[0].to_string().contains("missing catalog tool"));
    }
}
