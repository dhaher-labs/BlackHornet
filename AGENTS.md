# AGENTS.md — BlackHornet

## 1. Project Identity

| Field | Value |
|-------|-------|
| Name | BlackHornet |
| Type | Autonomous agent colony platform — Python core + Rust runtime workspace (strangler-fig) |
| Version | 7.0.0 |
| Python | >=3.11 |
| Rust | >=1.85, edition 2024 (`rust/` workspace; infinity pinned `=0.2.0`) |
| License | MIT |
| Owner | Dhaher Labs |

## 2. Architecture Summary (v7 reality)

```
src/
├── agents/          agent implementations (browser, coder, colony, executor, babyagi, …)
├── auto_discovery/  capability discovery
├── bh_types/        ecosystem-surface types (canonical EcosystemState)
├── browser/         browser agent
├── colony/          ECOSYSTEM CORE — orchestrator, radar (UDP), mesh bus, queen, jeumpa_brain
├── colony_types/    canonical core types (duality with bh_types — docs/11-adr/ADR-002)
├── config/          Pydantic settings
├── core/            consensus, debate arena, ast_sanitizer, self-healing, llm_provider,
│                    circuit_breaker, skill_crystallizer
├── graph/           SQLite property graph + session recorder
├── integration/     unified_launcher (bh-colony entrypoint)
├── memory/          SQLite+FTS5 7-module memory system + MemoryFederation
├── omega/           governance loop + council (EXPERIMENTAL — simulated debate)
├── plugins/, sandbox/, security/, thinking/, swarm/, tools/, ui/
├── hermes_quant.py  paper-trading main loop (L1–L5)
└── immortal_daemon.py, watchdog.py, swarm_protocol.py, colony_bridge.py,
    memory_bridge.py, upgrade_loop.py, user_alignment.py   # ops line

rust/                Phase 1–5 workspace: bh-rtd daemon, bh-bridge-uds (BH bridge
                     protocol v1 over UDS), bh-mcp-bridge (Infinity wiring);
                     BH_RUNTIME=python stays the default (docs/ARCHITECTURE.md §4.1)
tests/               pytest suite — the hard gate for every change
scripts/             infrastructure scripts (incl. gen_arch_graph.py)
docs/                numbered doc tree; canonical architecture: docs/ARCHITECTURE.md;
                     generated dependency graph: docs/02-arch/MODULE_GRAPH.md
deploy/              service files; Dockerfile / docker-compose.yml for containers
```

Pre-v7 lineages (`harness/`, `api/`, `mcp/`, `skills/`, 102 template agents, Node
`.mjs` islands) were **removed by the v7 rebuild** — do not reintroduce them;
per-item rationale lives in `PLAN.md` §2 and `docs/ARCHITECTURE.md` §8.

## 3. Build and Run

```bash
# Install in editable mode
pip install -e .

# Run unified colony launcher
bh-colony

# Run Omega CLI (experimental, simulated loop)
bh-omega

# Run trading subsystem (paper-only)
python -m src.hermes_quant

# Run tests (the gate)
pytest tests/ -q

# Run tests with coverage
pytest tests/ --cov=src

# Rust workspace (Phase 1–5)
cd rust && cargo build && cargo test
```

## 4. Code Conventions

### Imports
- Standard: `from src.tools.xxx import YYY`
- Wrong: `from tools.xxx import YYY` (missing `src.` prefix)
- Order: stdlib, third-party, local (blank line between each group)

### Type Hints
- Required on all function signatures
- Use `Optional`, `Dict`, `List` from `typing`

### Cross-Platform Paths
- Use `os.path.expanduser("~")` -- never hardcode `/sdcard/` or `/root/`
- Use `platform.system()` for OS detection, not `uname -o`
- Use `platform.node()` for hostname, not `os.uname().nodename`

### Logging
```python
logger = logging.getLogger("blackhornet.ModuleName")
```

### Communication
- All inter-agent communication uses JSON

### Comments
- Only when the WHY is non-obvious
- No comments that explain WHAT the code does
- No comments for hypothetical future needs

### Docs hygiene
- `docs/02-arch/MODULE_GRAPH.md` is GENERATED: after any structural change run
  `python3 scripts/gen_arch_graph.py` and commit the result; do not edit by hand
- Architecture claims go in `docs/ARCHITECTURE.md` under the honesty rules
  (simulated components are labeled simulated; no feature described as more than it is)

## 5. Key Patterns

- **Risk limits** are enforced in `src/tools/risk_officer_tool.py` -- never override these
- **Kill switch** (`src/tools/kill_switch_tool.py`) is fail-closed -- blocks by default when uncertain
- **SharedState** (`src/tools/shared_state.py`) is the single source of truth for runtime state
- **Trading tools** follow a layered architecture (L1 data through L5 execution) and are **PAPER-ONLY**
- **Omega Engine** runs an autonomous self-improvement loop -- experimental; the plan→execute loop and council votes are simulated
- **Circuit breaker** (`src/core/circuit_breaker.py`) prevents cascading failures
- **Type duality**: `colony_types/` = canonical core, `bh_types/` = ecosystem surface (ADR-002) — do not merge them casually

## 6. Security Rules

1. Never commit secrets (`.env`, API keys, tokens, credentials)
2. All user input must be validated before processing
3. LLM output is untrusted -- validate before use in logic or tool calls
4. External API calls must use `httpx` with explicit timeouts
5. Report vulnerabilities to mulkymalikuldhr@agentmail.to -- do not open public issues

## 7. Git Workflow

- Commit format: `type(scope): description` (feat, fix, docs, chore, test)
- Atomic commits: one logical change per commit
- No force-push to shared branches
- Tag releases with semantic versioning (MAJOR.MINOR.PATCH)
- Tests must pass before merging (`pytest tests/ -q`)

## 8. What NOT to Do

- Don't use `from tools.xxx` -- always `from src.tools.xxx`
- Don't hardcode Linux paths -- use cross-platform detection
- Don't add comments unless the WHY is non-obvious
- Don't create abstractions for hypothetical needs (YAGNI)
- Don't mix refactoring with feature work in the same commit
- Don't suppress warnings or ignore test failures
- Don't add new dependencies without checking if existing ones cover the need
- Don't describe simulated components as real (repo bans "fake success" patterns)
