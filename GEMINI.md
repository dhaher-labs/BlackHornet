# Gemini Guide — BlackHornet (v7)

BlackHornet is an autonomous agent colony platform (Python ≥ 3.11, package
`blackhornet-colony`): cooperating agent processes with federated memory, a
live ecosystem state exposed over HTTP, keyless LLM access, a sandboxed tool
registry, swarm-intelligence algorithms, and a **paper-only** trading stack.

Start here (in reading order):

- `AGENTS.md` — repo conventions and layout (source of truth for agents)
- `docs/ARCHITECTURE.md` — module map, mermaid diagrams, honest per-subsystem verdicts
- `docs/02-arch/MODULE_GRAPH.md` — generated internal import/dependency graph
- `PLAN.md` — rebuild plan + Rust (bh-rtd) roadmap
- `tests/` — pytest suite; run `python3 -m pytest -q` before any push

Honesty rules: no component is described as more than it is; simulated or
experimental subsystems (e.g. the omega council debate) are labeled as such
in the docs. Keep it that way.
