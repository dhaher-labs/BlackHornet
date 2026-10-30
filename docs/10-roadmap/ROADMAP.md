# Roadmap — BlackHornet

## Current (v7.0.0 — 2026-10-30)
- pyproject version 7.0.0 (CHANGELOG 2026-10-27); main @ 154dc5c (105 commits ahead of tag v7.0.0 = 93f745e)
- Test suite: 533 passed, 28 skipped (full run 2026-10-30, zero failures)
- Autonomous ecosystem runtime live: supervisor v2-hardened + keepalive daemon (10-min beats) + 5-min health watchdog; auto-restart on crash via nested-orphan protocol (heal verified 2x; heals_total = 1)
- Remote fleet in sync: 3 GitHub (mulkymalikuldhaher, mulkymalikuldhrs, org dhaher-labs) + 2 Codeberg (mulkymalikuldhr, org Dhaher-Labs) all at main=154dc5c + tag v7.0.0; GitLab re-ship pending old-project deletion watch
- Paper-trading only; posture per PLAN.md: `BH_RUNTIME=python` default, money-path excluded from runtime migration until Phase 5+ (live-trading gate + journal semantics regression first)

### Post-MVP engineering backlog (Phase 5, M2+)
1. **base_agent typing redesign** — mypy reports 1 known error rooted in base_agent type structure; requires a type redesign, not a quick fix (design task, then sweep)
2. **Host bootstrap wire** — AgentRegistry wiring decision for orchestrator + parked-agents bootstrap path (needs a design decision before implementation)
3. **Backpressure heartbeat** — optional hardening; no spec yet, spec-first
4. **Rust migration wedges** — see PLAN.md phased approach (stable seams: MCP, RAP, `ModelProvider`, store traits)

## Legacy snapshot (v5.0.0 — 2026-07-25)
- Omega Engine fully operational (12 modules)
- 21 trading tools (L1-L5 layers)
- 104 agents (9 core + 38 audit + 42 specialist)
- 348 tests passing, 0 failures
- CI/CD pipeline live (GitHub Actions)
- Windows + Linux compatible
- Import paths fixed (39 broken imports resolved)
- Dead modules cleaned (organism, bridges, plugins)

## Next (v5.1.0 — Integration Phase)

### Phase A: A-Evolve Imports (Python, directly copyable)
- [ ] VersionControl — git workspace versioning
- [ ] AgentWorkspace — filesystem contract
- [ ] Observer — JSONL observation collection
- [ ] EGL Metrics — evolution monitoring
- [ ] Types — SkillMeta, Observation, Task, Trajectory

### Phase B: MUE-X Imports (Python, adaptation)
- [ ] AST Mutation Engine — 6 mutation strategies
- [ ] DNA Genome — SHA256 integrity
- [ ] Autonomous Drives — 7 self-motivation signals
- [ ] PAD Emotions — behavioral dynamics
- [ ] Strategy Evolver — trading parameter mutation
- [ ] Memory Lattice — 6-layer SQLite FTS5
- [ ] MCP Plugin Creator — dynamic tool generation
- [ ] Domain Specialization — `/specialize trading`

### Phase C: Loop-Engineering Ports (TypeScript → Python)
- [ ] Circuit Breaker — stagnation + trigram similarity
- [ ] Path Gates — denylist + allowlist
- [ ] Readiness Audit — L0-L3 scoring
- [ ] Cost Estimation — token budgets
- [ ] Worktree Locking — deadlock detection
- [ ] Drift Detection — config sync
- [ ] MCP Discovery Server

### Phase D: Pixel-Agents Patterns (TypeScript → Python)
- [ ] HookProvider — multi-agent abstraction
- [ ] Session Router — event buffering
- [ ] Server Registry — PID liveness
- [ ] Dual-Mode Detection — hooks + polling
- [ ] Agent Teams — lead+teammate

## Medium-term (v5.2.0)
- Multi-instance swarm (L8)
- Docker deployment with docker-compose
- Full docs 00-21 complete with content
- CLI interface for all BH commands
- Custom skill marketplace
- Score threshold: 100/100/100

## Long-term (v6.0.0)
- Autonomous codebase refactoring
- Cross-project knowledge synthesis
- Predictive ecosystem management
- Multi-instance swarm intelligence
- Self-directed learning and skill acquisition
