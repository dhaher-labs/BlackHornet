# Changelog

All notable changes to BlackHornet are documented here.
Format based on [Keep a Changelog](https://keepachangelog.com/); versioning: SemVer.

## [7.0.0] — 2026-10-27

Clean rebuild of the project ("re-mapped, re-wired, re-documented, re-shipped").
Everything below was verified against the code and the test suite before shipping.

### Changed
- **Fresh git history.** The repository now starts from a single orphan commit.
  All pre-v7 history was retired: a credentials file had been committed to it and
  pushed to public remotes, so the history itself was treated as compromised.
  Fresh clones are small, fsck-clean, and contain no secrets.
- **Version** `5.1.0` → `7.0.0` (package `blackhornet-colony`); pydantic pinned to `>=2,<3`
  and v1-style model code migrated.
- **Honest documentation.** README and docs/ARCHITECTURE.md now state per-subsystem
  verdicts backed by evidence: what is real, what is paper-only, and what is
  simulated. Marketing claims (AGI, "immortal", "104 agents", "full MCP",
  "15+ providers") were removed.

### Fixed
- Wiring repairs, each gated by the test suite (366 passed / 22 skipped baseline):
  import-time crashes resolved, the no-op task-queue stub in the role orchestrator
  replaced with fail-loud behavior, `hermes_quant` given a proper `__main__` guard,
  test code no longer shipped inside the wheel, runtime state (`data/`, `exports/`,
  `*.db`) untracked and gitignored.

### Removed
- Dead or unimportable subsystems (dropped, not deleted from history — they are
  simply no longer part of v7): the deer-flow harness port (undeclared
  langchain/langgraph), the orphaned FastAPI stack, the crashing channels and MCP
  modules, Node.js `.mjs` islands, a duplicated type system, the twin
  `integrations/` directory, 102 template agent files, aspirational bridges to
  external repos, loose daemon scripts, and root marketing/status documents.
- Runtime artifacts (`data/messages/*`, session exports, SQLite DBs) from version
  control. The legacy `ui/` game and `skills/` packs are separated from the
  product tree.

### Security
- History retirement removes all pre-v7 secrets from the repository. **Rotation of
  the exposed credentials remains a required user action** — removing them from
  history does not un-leak them. Wallet keys in particular must be treated as
  burned and swept to new addresses.
- Fake-success pattern banned: modules must return explicit degraded/failed states,
  never plausible-looking canned strings (CI grep gate added).

## Legacy (v5.x – v6.x)

All releases before v7.0.0 shipped from a git history that has been retired for
security reasons (see above). There are no legacy release notes: pre-v7 history
contained marketing claims that audit showed to be stubs or dead code, so those
claims were withdrawn rather than carried forward. The v5.x–v6.x line ends at the
pre-rebuild commit; v7.0.0 is the new root.

[7.0.0]: #700---2026-10-27
