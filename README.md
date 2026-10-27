# BlackHornet

![License](https://img.shields.io/badge/license-MIT-green)
![Python](https://img.shields.io/badge/python-3.11%2B-blue)
![Version](https://img.shields.io/badge/version-7.0.0-orange)
![Trading](https://img.shields.io/badge/trading-paper--only-red)

> **Status — v7.0.0 clean rebuild.**
> BlackHornet v7 is a fresh rebuild of the project with a **new single-commit git history** (all pre-v7 history was retired for security reasons — see [Security](#security)).
> The trading module is **paper-trading only**. There is **no live broker connectivity**. Nothing in this repository is financial advice.

---

## What BlackHornet is

BlackHornet is an **autonomous agent colony platform** written in Python: a colony of cooperating agent processes with shared memory, a live ecosystem state, keyless LLM access, a sandboxed tool registry, and a paper-only trading stack — served through a zero-dependency dashboard.

It is **not** an AGI, and it does not self-improve in any meaningful sense yet. Parts of it (the Omega "self-improvement" loop and the Council debate) are **governance scaffolding with simulated debate output** and are labeled as such. This README describes what actually works, verified against the code and a 366-test suite at the v7 baseline.

## Working subsystems

| Subsystem | What it does | Status |
|---|---|---|
| **Memory federation** | SQLite + FTS5 memory system (STM/LTM/semantic/episodic) with `MemoryFederation`: content-hash dedupe, confidence-weighted merge, importance-ranked retrieval, TTL pruning | Real, tested |
| **Colony ecosystem** | `EcosystemOrchestrator` sync loop; `ColonyRadar` UDP peer discovery; `MeshBus` messaging; `SelfNest` health/restart; `QueenAgent` command surface. Multi-instance mesh is exercised by unit tests only — no second colony ships with the repo | Real, single-node |
| **Jeumpa Brain** | Keyless LLM layer: 5 public community endpoints with task profiles, cooldowns, and response caching. Third-party free endpoints — availability and quality are not guaranteed | Real, best-effort |
| **Tool registry** | File / Shell / Code tools behind a registry with circuit breaker, rate limiting, and path sandboxing | Real |
| **Risk officer** | Hard veto on every trade: max 0.5% risk/trade, 1%/day, 3%/week; vetoes are logged, never bypassed | Real, paper-only |
| **Trading stack** | yfinance market data → technical analysis → risk veto → **paper** execution → SQLite journal. No broker code exists anywhere in the tree | Real, paper-only |
| **Ecosystem state API** | `GET /api/v1/ecosystem` — canonical, audited `EcosystemState` (agents, tasks, debates, audit log) | Real |
| **Dashboard / UI** | Zero-dependency stdlib HTTP dashboard (port 8080) with API bridge and WebSocket events | Real |
| **Swarm intelligence** | PSO (4 topologies), ABC, stigmergy — pure algorithm libraries | Real, math only |
| **Knowledge graph** | SQLite property graph + session recorder, wired into the dashboard | Real |
| **Self-healing** | Phi-accrual failure detector | Real |
| **Omega Engine / Council** | "Self-improvement" loop + 15-persona consensus council. The consensus math engine is real; **the debaters are simulated** — votes and debate text are template strings, the loop produces no real work | **Experimental / simulated** |
| **SAMAN governance** | Consensus engine (score ≥ 0.85, 3 rounds, fail-closed), debate arena, AST sanitizer. Sound engine, awaiting real (LLM-backed) debaters | Engine real, wiring experimental |

## Quickstart

Requirements: **Python ≥ 3.11** and pip.

```bash
# install (editable, local package "blackhornet-colony")
pip install -e .

# dashboard + ecosystem API + memory + graph (http://localhost:8080)
bh-colony

# Omega governance loop (EXPERIMENTAL — simulated debate, see table above)
bh-omega

# paper-trading loop (long-running; market data via yfinance, alerts only if
# TELEGRAM_* env vars are set — no broker connectivity exists)
python -m src.hermes_quant

# run the test suite
pip install -e ".[dev]"
pytest tests/ -q
```

LLM and alert credentials are read from environment variables only; none are required to run the dashboard. Never commit `.env` files — pre-v7 history contained one, which is why the history was retired.

Docker: a multi-stage non-root `Dockerfile` and `docker-compose.yml` (services `omega`, `omega-loop`) are included; `deploy/` holds systemd units and container entrypoints.

## What v7 removed

Dropped in the rebuild (dead, broken, or decorative — see [PLAN.md](PLAN.md) for the full rationale): the undeclared-dependency `src/harness` port, the orphaned FastAPI stack (`src/api`), the import-crashing `src/channels` and `src/mcp` modules, Node.js `.mjs` islands, a duplicated type system, 102 template agent files, runtime data committed to git, and root marketing documents. The legacy `ui/` "Living World" game and `skills/` packs are being separated from the product tree.

## Architecture

See [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) for the module map, mermaid dependency graph, runtime data flows (agent turn, paper-trading turn), persistence layer, and honest per-subsystem verdicts.

## Roadmap

Post-shipping, Python stays the default runtime (`BH_RUNTIME=python`); a Rust core is grown beside it behind stable seams (MCP, RAP, `ModelProvider`, store traits). Posture: *adopt the protocol, embed the runtime, never port the colony*.

| Phase | Weeks | Goal |
|---|---|---|
| 0 | 0–2 | Ground truth: adapter contracts; BH tool round-trips over a RAP endpoint |
| 1 | 2–6 | Bridges before ports: Infinity MCP bridge + `JeumpaProvider` implementing `ModelProvider` |
| 2 | 6–12 | Runtime wedge: `bh-rtd` Rust daemon embedding `infinity-agent-core` behind `BH_RUNTIME=rust` |
| 3 | 10–16 | Memory seam: Infinity store traits backed by BH SQLite/FTS5 memory |
| 4 | 14–22 | Native RAP tool servers in Rust (shell/file/search first) |
| 5 | 20–30 | Hardening & UI: watchdog timers in Rust, view updates into the dashboard |

The trading money-path is **excluded from migration until Phase 5+**. Full detail: [PLAN.md](PLAN.md) §4.

## Documentation

- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — architecture map (v7)
- [PLAN.md](PLAN.md) — rebuild plan, roadmap detail, acceptance criteria
- [docs/13-changelog/](docs/13-changelog/CHANGELOG.md) — historical changelog and SAMAN integration audit
- [README_id.md](README_id.md) / [README_zh.md](README_zh.md) — translations predate v7 and may be outdated

## Security

- **Fresh history:** all pre-v7 git history was retired because a credentials file had been committed to it and pushed to public remotes. Anyone with a pre-v7 clone must treat those credentials as compromised and rotate them.
- **Working tree:** kept clean of secrets by policy; credentials come from environment variables. If you find a secret in the tree, do not open a public issue — contact the maintainers.
- **Trading safety:** the execution path is paper-only by construction; there is no broker adapter in the codebase.

## License

[MIT](LICENSE) — Copyright (c) 2026 Mulky Malikul Dhaher / Dhaher Labs contributors.
