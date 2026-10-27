# BlackHornet v7.0.0 Clean Rebuild & Shipping Plan

- **Task:** 12-e (planner) · **Date:** 2026-10-27 · **Repo:** /home/z/blackhornet @ 82fb90f
- **Sources:** agent-ctx/research-infinity-rust.md (researcher, 12-c), agent-ctx/architect-findings.md (architect, 12-d-1), agent-ctx/audit-report.md (auditor, 12-d-2), docs/ARCHITECTURE.md. Every claim below traces to one of these.
- **Locked decisions (not re-litigated):** fresh orphan git history (NO filter-repo); target v7.0.0; same 6 remote names recreated; secrets rotation is a USER action; tests stay green (366 passed baseline); `agent-ctx/` excluded from the shipped tree.

---

## 1. Executive Summary

**What/why:** BlackHornet is being rebuilt clean locally — re-mapped, re-wired, re-documented, tidied, and re-shipped as **v7.0.0** — because the current tree is a 5-lineage fusion (AMC core, dead deer-flow harness, Hermes quant, immortal-daemon shell line, colony ecosystem v2) whose headline features are stubs, dead, or unimportable, and whose git history carries a burned credentials file. Ship = recreate the 6 remotes and push a **fresh single-commit orphan history**, which permanently removes `.env.hermes` (~50 live secrets) from history. Future = strangler-fig Rust rewrite with hydro-project/infinity.

**Current state (5 bullets):**
1. **What runs:** only three things actually work — the stdlib dashboard + memory/graph stack (`src/ui/web_server.py` + `api_bridge.py`), the paper-only trading bot (`src/hermes_quant.py` + `src/tools/`), and the ecosystem-state API (`GET /api/v1/ecosystem`) — while Omega's self-improvement loop, the 15-persona council, harness agents, channels, and MCP are simulated, decorative, or unimportable (architect §1, §7, §9).
2. **Wiring truth:** live import sweep of 673 `src` modules = 496 OK / 177 FAIL; real bugs: `MessageFormat` NameError kills all 6 `src/channels` modules, pydantic v1/v2 clash kills all 8 `src/mcp` modules, `src/api` FastAPI stack has zero callers, `src/harness` (224 files) needs langchain/langgraph that requirements.txt never declares (architect §1, B1–B7).
3. **Secrets verdict:** working tree CLEAN (0 token hits), but `.env.hermes` with ~50 live secrets (GitLab/Codeberg PATs, 7 Telegram bots, 5 Groq keys, MetaMask/Phantom/Steem private keys) sits in **36 history commits already pushed to 5 public remotes** — all credentials must be treated as compromised (auditor F1).
4. **Hygiene:** 1,491 of 2,435 tracked files (61%) are runtime state under `data/` (+ tracked SQLite DBs, `qna-cache.json`, `exports/`); `.gitignore` lacks `data/`, `exports/`, `*.db` rules; the clone's object store is corrupted (153 broken links, 55/162 commits unreadable) (auditor F2, F3).
5. **Tests & packaging:** 366 passed / 22 skipped / 0 failed verified independently twice (architect §7, auditor §4); packaging is valid (pyproject OK, `bh-colony`/`bh-omega` entry points real, Dockerfile non-root) with one gap — pydantic pin `>=1.10,<3` tolerates v1-style code that crashes on installed v2.12.5; `python -m src.hermes_quant --help` boots the live system and dials Telegram (auditor F4, F6, §6).

---

## 2. Scope v7.0.0 — DROP / KEEP / FIX (file-level)

### 2.1 DROP (13 items) — validated against architect-findings §4, §6.5

| # | Item | Rationale | Risk |
|---|------|-----------|------|
| D1 | `src/harness/` (224 files) | Dead in any requirements-driven env — needs undeclared langchain/langgraph (B5) | Lose deer-flow patterns; mirror backup + branch quarantine preserves them |
| D2 | `src/api/` (14 files, FastAPI stack) | Orphaned: `create_fastapi_app()` has zero callers; real API is `ui/api_bridge.py` (B4) | Lose REST surface → v7 keeps api_bridge; one FastAPI mount is a post-v7 option |
| D3 | `src/channels/` (6 files) | All 6 crash at class-definition time (`MessageFormat` NameError, B1); "worse than absent" — import-crash poisons consumers | Re-implement channels later on a working base, not a broken one |
| D4 | `src/mcp/` (8 files) + `src/agent_tools/mcp_tool.py` | Pydantic v1 `Config` + v2 `ConfigDict` clash makes them unimportable (B2); Infinity's MCP bridge (roadmap P1) supersedes it | README's "full MCP" claim dies — replace with honest docs |
| D5 | `src/llm_router/*.mjs`, `src/delta/*.mjs` (Node islands) | Unreachable from Python; `llm_router` has zero Python files (architect §3) | None — nothing imports them |
| D6 | `src/colony_types/` (11 files) | Duplicate type system vs `bh_types/`; `messages.py` byte-identical (auditor F5); keep `bh_types` (EcosystemState is the live canonical) | Rewrite imports in radar/mesh/nest consumers of `colony_types` |
| D7 | `src/integrations/` (8 files) | Twin dir of `src/integration/` (which hosts the live unified_launcher); its clients (crucix/organism/quant bridges) serve only the orphaned ecosystem router (architect §10) | Re-add deliberately if external bridges are ever needed |
| D8 | 102 template agents (`src/agents/audit/*` 51 + `specialists/*` 51) | Near-identical name-swapped files, zero unique behavior beyond prompts (architect §3) | Keep the core role set (planner/coder/researcher/executor + colony orchestration) per architect §6 |
| D9 | `skills/` (~20 SKILL.md packs) | Tooling artifacts, not product code (architect §4.6) | Move to a separate assets location, not deleted |
| D10 | `scripts/` loose daemons (blackhornet_daemon, autonomy, keeper, message_bus, self_write, rebrand, push-all, auto-push) | Overlap with `src/immortal_daemon.py`/`src/watchdog.py`/`swarm-config/` (architect §4.7) | Consolidate into one ops surface; keep `hermes.sh` references working |
| D11 | `src/hermes_nous/layers.py`, `src/hypergate.py`, `src/mnemosyne_bridge.py` | Aspirational glue to repos not part of this codebase (architect §4.9) | None at HEAD; they are unwired |
| D12 | Root marketing/status MDs (`PR.md`, `WEALTH_REALITY_CHECK.md`, `FINAL_REALIZATION.md`, `PROXYGATELLM_*`, `UPGRADE_PLAN.md`, `SESSION-SUMMARY.md`, …) | Historical noise (architect §4.10) | Archive under `docs/history/` (move, not delete) |
| D13 | `ui/` "Living World" game (second UI) | Two UIs, two ports, two ideas of "status" (architect §3, §7) | Move to its own repo; keep `src/ui/` as the one dashboard |

### 2.2 KEEP (15 items) — the real gems (architect §5, verified by read + test run)

| # | Item | Why |
|---|------|-----|
| K1 | `src/memory/` — 7-module SQLite+FTS5 system + `MemoryFederation` (dedupe/merge/importance/TTL) | Real, tested; the memory seam for Rust future |
| K2 | `src/colony/` core — EcosystemOrchestrator sync loop, ColonyRadar (UDP), MeshBus, SelfNest, QueenAgent | Live ecosystem core with file:line evidence |
| K3 | `src/colony/jeumpa_brain.py` — 5 real keyless providers, task profiles, cooldowns, cache | Zero-cost LLM layer; top integration fit for Infinity `ModelProvider` |
| K4 | `src/bh_types/ecosystem.py` EcosystemState + `GET /api/v1/ecosystem` via api_bridge | Canonical audited state; SAMAN phases 3–5 wiring |
| K5 | `src/core/consensus_engine.py` (S_c≥0.85, 3 rounds, fail-closed) + `debate_arena.py` + `ast_sanitizer.py` | Sound governance engine — needs real debaters, not new math |
| K6 | `src/agent_tools/` FileTool/ShellTool/CodeTool + registry (circuit breaker, rate limit, permissions) | Complete, sandboxed, modulo B2/B3 fixes |
| K7 | `src/tools/` trading stack + `src/hermes_quant.py` (yfinance data, TA, hard-veto RiskOfficer 0.5%/1%/3%, PAPER execution, SQLite journal) | Real, paper-only, no broker code anywhere |
| K8 | `src/swarm/` PSO/ABC/stigmergy (numpy + pure-python shim) | Real math, no Infinity counterpart — stays Python |
| K9 | `src/graph/graph_store.py` SQLite property graph + session recorder | Live and wired into api_bridge |
| K10 | `src/core/self_healing.py` phi-accrual detector | Real math, tested |
| K11 | `src/ui/web_server.py` + `api_bridge.py` | Zero-dep serving that actually boots — the ONE UI in v7 |
| K12 | Entrypoints `bh-colony` (unified_launcher), `bh-omega` (omega.cli) | Verified working `--help` (auditor §7) |
| K13 | `tests/` suite — 23 files, 366 passed / 22 skipped baseline | The green gate for every tidy step |
| K14 | Packaging: `pyproject.toml`, Dockerfile (multi-stage, non-root), `deploy/`, `hermes.sh`, `test.sh` | Dry-run verified; docker-compose path exists |
| K15 | Docs: `docs/ARCHITECTURE.md` + numbered doc tree (changelog, SAMAN audit) | The re-mapped documentation of record |

### 2.3 FIX (9 items) — wiring repairs & hygiene, each gated by the 366-test suite

| # | Item | Rationale | Risk |
|---|------|-----------|------|
| F1 | **Pydantic pin decision: pin `pydantic >=2,<3`** and migrate remaining v1-style `class Config` to `model_config` | Installed v2.12.5 breaks v1-style code (auditor §6 gap, F4); v1/v2 duality is the root of B2 | Touches every model consumer; suite + import sweep gate it |
| F2 | `src/hermes_quant.py` `__main__` guard (argparse `--help`) | Today `--help` boots the live loop and dials Telegram (auditor F6) | Low; behavior-only guard |
| F3 | Untrack runtime state: `git rm -r --cached data/ exports/`; `.gitignore += data/, exports/, *.db, *.sqlite` | 61% of tracked files are runtime state; bots re-ingest state (auditor F3) | Low — data is regenerable; keep a local backup before untracking |
| F4 | Fix `browser_tool` import: `apply_stealth` name mismatch (B3) | Unimportable module in an otherwise-real package | Low; or drop the legacy shim per architect §6 |
| F5 | Replace `role_orchestrator.py:48-52` no-op `TaskQueue` stub with a real queue or fail-loud | Silent drop of enqueued work = fake success (B6) | Medium — first real behavior change; suite gate |
| F6 | Remove `src/*/tests*.py` from the shipped package (pyproject excludes / move to `tests/`) | Test code ships in the wheel today (B7) | Low; keep package self-test convention optional |
| F7 | Repair or fold `scripts/test_launcher.py`, `test_llm_router_bridge.py`, `test_mcp.py` | 3 collection errors (auditor F4); if D10 drops their subjects, delete with them | Low |
| F8 | Ban fake-success: orca/babyagi/council/`llm_provider` mock-mode must return explicit degraded/failed states, never plausible strings; add CI grep gate for "Simulated execution"-style returns | Architect §2.1 + §6 rule 4 — the repo's worst observability pattern | Medium — may surface that Omega loop does no real work; docs must say so honestly |
| F9 | Fix `unified_launcher` runpy import-order warning (auditor F8) | Cosmetic but shipped in the primary entrypoint | Trivial |

---

## 3. Ship Checklist v7.0.0 (ordered, executable)

1. **Tidy execution** — apply §2 DROP/KEEP/FIX on a working tree; re-run the import sweep after each DROP (baseline 496 OK shrinks only by dropped modules); archive D12 under `docs/history/`; add `agent-ctx/` to `.gitignore` (local-only audit material — never enters the new history).
2. **Tests green** — `pytest tests/ -q` must report **366 passed, 22 skipped, 0 failed** after every step; no tidy merges with a red suite (locked constraint).
3. **Version bump** — `pyproject.toml` 5.1.0 → **7.0.0**; README + changelog entry reflecting honest subsystem verdicts (architect §9 replaces marketing claims).
4. **Fresh orphan history** — `git checkout --orphan v7` → single initial commit `BlackHornet v7.0.0` (locked decision; supersedes filter-repo). Verify: exactly 1 commit; `git log --all -- .env.hermes` EMPTY; `git fsck --no-dangling` = 0 broken links (corruption F2 does not carry over — new objects only).
5. **Tag** — annotated `v7.0.0` on the orphan commit.
6. **Recreate 6 remotes** (exact names, high-level API calls; tokens are USER-held, agents never embed secrets):
   - GitHub: `POST /user/repos {"name":"BlackHornet"}` (→ `mulkymalikuldhaher/BlackHornet`), same for `mulkymalikuldhrs/blackhornet`; `POST /orgs/dhaher-labs/repos` (→ `dhaher-labs/BlackHornet`).
   - Codeberg (Forgejo): `POST /api/v1/user/repos` (→ `mulkymalikuldhr/blackhornet`); `POST /api/v1/orgs/Dhaher-Labs/repos` (→ `Dhaher-Labs/BlackHornet`). ⚠️ Codeberg personal quota was over at Task 11 (913.2/750 MiB) — user must free quota; the new history is tiny (~16 MB tree, no old objects).
   - GitLab: `POST /api/v1/projects?path=blackhornet&namespace_id=<user>` (→ `mulkymalikuldhr/blackhornet`); retry with backoff (anti-abuse pattern seen at Task 11).
7. **Push** — `git remote add` ×6 (gh1, gh2, gh3-org, cb1, cb2-org, gl) → push `main` + `v7.0.0` to each; fresh push only, no force-push needed (new remotes start empty).
8. **Post-ship verification matrix** — per remote: `ls-remote` HEAD == orphan commit and tag present; fresh clone → `git fsck --no-dangling` clean; `git log --all -- .env.hermes` empty; token scan 0 hits; `pytest tests/ -q` 366/22; `bh-colony --help` and `bh-omega --help` exit 0; `agent-ctx/`, `data/`, `exports/` not in `git ls-files`.
9. **SECRETS ROTATION — USER ACTION (agents cannot do this):** rotate all ~50 `.env.hermes` credentials (GitLab PAT, Codeberg token, 7 Telegram bot tokens, 5 Groq keys, NVIDIA, Nexus, Together, AlphaVantage/Polygon/financial APIs, HF read+write, …) and **sweep crypto wallets to new addresses** (MetaMask, Phantom, Steem owner/active keys are burned, not rotatable) — auditor F1/§8.1. Fresh history removes the repo copy, NOT the exposure.

---

## 4. Rust + Infinity Roadmap (strangler-fig, from research-infinity-rust.md §3)

Posture: **adopt the protocol, embed the runtime, never port the colony.** Python BlackHornet stays fully working at every phase; `BH_RUNTIME=python|rust` flag gates every wedge, **`BH_RUNTIME=python` is the default**; the **trading money-path is excluded from migration until Phase 5+** (live-trading gate + journal semantics regression-tested before any runtime flag flip). Stable seams: MCP, RAP, `ModelProvider`, store traits.

| Phase | Goal | Deliverable | Exit criteria |
|-------|------|-------------|---------------|
| 0 (wk 0–2) | Ground truth | Adapter contract doc (message/tool/memory schemas) + `src/bridge/infinity/` calling a RAP endpoint | BH tool round-trips via `rap-test-servers` |
| 1 (wk 2–6) | Bridges before ports | `infinity-mcp-bridge` before BH MCP servers + `JeumpaProvider` implementing `ModelProvider` (UDS provider may stay Python) | An Infinity agent with 100% BH brain + tools |
| 2 (wk 6–12) | Runtime wedge | `bh-rtd` Rust daemon embedding `infinity-agent-core` (threads, `spawn_thread`≈sub_agent pool, durable timers) | Thread execution runs in Rust behind `BH_RUNTIME=rust`; Python default intact |
| 3 (wk 10–16) | Memory seam | Infinity `ConversationStore`/`StateStore` over BH SQLite FTS5 (+ ChromaDB read-through) | Hibernating agents persist into BH MemoryFederation |
| 4 (wk 14–22) | Native RAP tools | shell/file/search as native Rust RAP tool servers (NOT docker/browser) | Those tools bypass the MCP proxy; rest keep proxy |
| 5 (wk 20–30) | Hardening & UI | Watchdog/heartbeat via `sleep_until_event_or_input` + subscriptions; `view_update` → `src/ui/server.py` | Python/Rust parity checks; SAMAN/Omega remain Python |

Top roadmap risks (research §3.4): pre-1.0 upstream churn (pin `=0.2.0`, adapter layer), non-`Send` local tasks (LocalSet, single-threaded per system), AWS-gravity serverless mode (use local mode + rig providers), hibernation vs. BH immortality semantics (resident daemon for colony, hibernation for scheduled roles).

---

## 5. Risk Register (top 10) & Acceptance Criteria

| # | Severity | Risk | Mitigation |
|---|----------|------|------------|
| R1 | CRITICAL | Live credentials stay exposed until user rotates (history copies may persist in old clones/forks) | Rotation is checklist step 9, flagged for USER before/parallel to push; fresh history removes the repo copy |
| R2 | HIGH | Tidy step breaks imports or tests | 366-test gate after every step; import sweep re-run per DROP; drops applied atomically per subsystem |
| R3 | HIGH | Hidden importer of a dropped module surfaces late | Re-run full import sweep post-tidy; any FAIL on kept modules blocks ship |
| R4 | MEDIUM | Orphan-history rebuild goes wrong (extra commits, tracked junk, leaked paths) | Pre-push checks: 1 commit, `git log --all -- .env.hermes` empty, `git ls-files` excludes `agent-ctx/`, `data/`, `exports/`, fsck clean |
| R5 | MEDIUM | Remote creation blocked (Codeberg quota over at Task 11; GitLab anti-abuse throttling) | User frees Codeberg quota first; backoff retries; push remotes independently so one failure doesn't block five |
| R6 | MEDIUM | Pydantic v2 pin (F1) regresses kept models | Pin `>=2,<3`, migrate v1 `Config` case-by-case, suite gate per commit |
| R7 | MEDIUM | Fake-success pattern survives tidy and misleads again | F8 explicit-degraded rule + CI grep gate for simulated-return strings |
| R8 | MEDIUM | Scope creep — tidy turns into a rewrite | v7 scope frozen to §2 lists; all Rust work stays in §4 phases, post-ship |
| R9 | LOW | Untracked `data/`/`exports/` loses something needed | Runtime state is regenerable; local backup before `git rm --cached`; exports archived |
| R10 | LOW→MEDIUM | Infinity pre-1.0 churn bites the future roadmap | Pin exact crates, isolate Infinity types behind `bh-rtd` traits, `BH_RUNTIME=python` default forever-available |

**Acceptance criteria — "shipping ready" (binary, checkable):**
- [ ] `git rev-list --count HEAD` == 1 on a fresh clone of each remote (single orphan commit)
- [ ] `git log --all -- .env.hermes` → empty output on all 6 remotes
- [ ] `git fsck --no-dangling` → 0 broken links on fresh clones (F2 corruption gone)
- [ ] Token-format scan (`ghp_`, `glpat-`, `sk-`, `AKIA`, `xox[brpsa]-`, private-key PEM) over fresh clone → 0 hits
- [ ] `pytest tests/ -q` → **366 passed, 22 skipped, 0 failed**
- [ ] `bh-colony --help` and `bh-omega --help` exit 0; `python -m src.hermes_quant --help` prints usage with zero network calls
- [ ] `pyproject.toml` version == 7.0.0; `git ls-remote` shows `v7.0.0` tag on all 6 remotes at the same commit
- [ ] `git ls-files` contains none of: `agent-ctx/`, `data/`, `exports/`, `src/harness`, `src/api/`, `src/channels`, `src/mcp`, `*.mjs`
- [ ] Import sweep of kept `src` modules → 0 failures; CI grep gate active for simulated-return strings
- [ ] User confirms credentials rotation started (wallets swept) — final human sign-off

*End of plan. Under 250 lines; no secrets; all claims trace to researcher/architect/auditor reports.*
