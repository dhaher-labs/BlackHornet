# DOMAIN_CATALOG — Katalog Domain (Artefak B0)

## Status
Accepted (2026-10-31) — bagian dari fase **B0 Konsolidasi + Audit** (ATLAS_PLAN.md §7).

## Tujuan & aturan pemisahan
Katalog ini memisahkan tiga lapisan yang di gate B0 **dilarang dicampur** menjadi satu
konsep (ATLAS_PLAN.md §7):

1. **Domain operasional** (organisasi — D8, D10): bidang kerja yang didefinisikan sebagai
   data konfigurasi, bukan kode runtime.
2. **Domain arsitektur** (kemampuan platform): subsistem teknis yang dibangun di crate/level
   kode, independen dari domain operasional mana pun.
3. **Runtime** (eksekutor — D2, D3): mesin yang mengerjakan task; di-balik adapter, tidak
   pernah menjadi identitas domain operasional.

Konsekuensi praktis: mendefinisikan domain operasional baru TIDAK membutuhkan runtime baru;
memasang runtime baru TIDAK mengubah katalog domain. Definisi ≠ instance (ATLAS_PLAN.md §6).

## 1. Katalog domain operasional (D10)

Delapan domain penuh (D10: "PABS = Labs, domain penuh, bukan sub-divisi"). Setiap domain
punya workflow, standar artefak, dan evaluasi sendiri (gate B7: "tiap domain menghasilkan
artefak+bukti sesuai standar domainnya").

| Domain | Ruang lingkup | Catatan grounding |
|--------|---------------|-------------------|
| **Labs** | Riset & eksperimen (eks-PABS) | Domain penuh per D10 |
| **Quant** | Riset trading & evaluasi strategi | **Paper-only** sampai Phase 5+ (D9); live-trading gate + regresi jurnal dulu |
| **Engineering** | Repo ops, build, test, ship | Fleet 5 remote + gerbang pytest/mypy/ruff sebagai standar evaluasi awal |
| **Media** | Konten & publikasi | — |
| **Business** | Operasi bisnis | — |
| **Community** | Interaksi komunitas | — |
| **Personal** | Asisten pribadi | — |
| **Security** | Keamanan & kepatuhan | Konstitusi `Dhaher_Labs_Constitution_v1.0.md` sebagai dasar kebijakan |

Aturan: penambahan domain baru = entri konfigurasi tervalidasi (D8), ditolak saat load jika
invalid; tidak ada domain tanpa standar artefak + evaluasi.

## 2. Peta domain arsitektur target vs aset sekarang

Dua belas subsistem platform (ATLAS_PLAN.md §3, fakta repo per 2026-10-30 @ main `66f279a`):

| # | Domain arsitektur | Aset sekarang | Gap utama |
|---|-------------------|---------------|-----------|
| 1 | Human Control & Experience | CLI launcher (`sovereign.py`, `integration/unified_launcher.py`); dev-server UI Next.js terpisah | Control room/dashboard terpadu, approval center |
| 2 | Identity & Governance | `AGENTS.md`, konstitusi (dokumen) | Registry identitas/role/policy yang di-enforce kode |
| 3 | Intelligence & Planning | `src/colony/orchestrator.py`, omega engine | Planner berbasis goal registry + world state |
| 4 | Orchestration & Work Mgmt | ecosystem runner, immortal_daemon (shell-level), scheduler cron eksternal | Task lifecycle state machine persisten, dependency graph, retry/recovery |
| 5 | Federated Agent Runtimes | colony runtime sendiri; `hermes_quant.py`; bridge `colony_bridge.py` | Adapter kontrak untuk OpenCode/Hermes/eksternal (D3) |
| 6 | Capability & Organization Engine | `src/swarm/parked_agents.py`, roles sebagai prompt | Role/capability registry sebagai data (D8); team composition |
| 7 | Tools & Access | `rust/crates/bh-mcp-bridge` (catalog + tool_bridge) — seed MCP ada | MCP Center penuh, Credential Center, execution gateway, policy enforcement |
| 8 | Knowledge & Memory | `src/memory`, `memory_bridge.py`, MemoryFederation (FTS5/ChromaDB per PLAN.md) | Integrasi Obsidian + librarian + provenance + decision register (D4) |
| 9 | Assets & Environments | Git lokal + 5 remote fleet (sync guardian) | Repository registry metadata, worktree per task, workspace isolation |
| 10 | Domain Operations | trading (paper-only, omega), docs/00–17 | Workflow domain end-to-end per pack |
| 11 | Evaluation & Evolution | pytest 533/28 sebagai gerbang; guardian harian | Evaluation registry per jenis kerja, benchmark, promosi capability |
| 12 | Rust Core Infrastructure | `rust/crates/bh-rtd` (daemon/host/infinity adapter), `bh-bridge-uds` (IPC) | Durable state, event log, budget, recovery — subsistem penuh (D1) |

## 3. Delapan domain sering terlupakan

Wajib ada di peta; tidak semua dibangun di M1 (ATLAS_PLAN.md §3):

Model Gateway · Trigger & Event Engine · Notification & Approval Center ·
Artifact & Provenance · Budget & Resource Manager · Evaluation Registry ·
Backup & DR · Integration Lifecycle.

## 4. Kaitan ke artefak B0 lain
- DECISIONS.md D8 (organisasi sebagai data) dan D10 (Labs = domain penuh) adalah dasar
  bagian 1; perubahan katalog operasional mengikuti konvensi register keputusan.
- RUNTIME_CONTRACT (berikutnya) harus menjaga lapisan 3 tetap terpisah dari lapisan 1-2.
- Bagian 2 adalah baseline gap; peta ulang dilakukan setelah tiap fase B-fase selesai.
