# BlackHornet → Institution Platform ("ATLAS") — Rencana Ulang v1.0

- **Baseline:** 2026-10-30 · repo @ `66f279a` (v7.0.0-105) · status: RENCANA, belum diimplementasikan
- **Sumber visi:** sesi strategi operator 28 Okt 2026 (ChatGPT share `6ae456bc-aebc-83ec-9dde-f25555bedfd8`) — dibaca utuh 30 Okt 2026
- **Relasi:** melengkapi `PLAN.md` (ship v7.0.0 + strangler-fig Rust) dan `ROADMAP.md` (snapshot v7.0.0). Tidak menggantikan keduanya.
- **Disclaimer:** tidak ada audit aset lama penuh yang sudah dilakukan; setiap klaim "sudah ada" di bawah diverifikasi langsung pada repo per 2026-10-30.

---

## 1. Inti visi (dari sesi operator)

Operator tidak kekurangan agent, prompt, atau ide divisi. Yang kurang adalah **HARNESS** — satu lapisan kendali di atas semua coding agent/runtime. Rencana lama (BlackHornet v1–v6, AMC, Autonomous Organism) diarsipkan karena yang dibangun adalah organisasi/persona, bukan lapisan koordinasi yang bisa mengelola pekerjaan dari awal sampai selesai.

Keputusan arah baru: **platform institusi AI** — satu control plane yang dapat mengoperasikan banyak organisasi (Personal OS → Project OS → Institution OS), menggunakan banyak runtime (federasi), membentuk tim dari registry capability, dan mengembangkan kemampuan lewat bukti terukur.

Tiga pemisahan yang wajib dijaga:
1. **Orchestrator bukan agent super** — mengatur task/dependensi/verifikasi, bukan mengerjakan semuanya.
2. **Runtime bukan divisi** — Hermes bisa dipakai banyak divisi; tidak ada runtime baru per jabatan.
3. **Organisasi bukan workflow kaku** — template + kebijakan; tim disusun per tujuan, di dalam batas izin/biaya/keselamatan.

## 2. Keputusan terkunci (operator, 28 Okt 2026)

| # | Keputusan | Catatan |
|---|-----------|---------|
| D1 | **Rust-first** untuk core control plane | Python BlackHornet tetap hidup (posture PLAN.md: `BH_RUNTIME=python\|rust`, default python) |
| D2 | **Federasi runtime** — satu orchestrator di atas banyak runtime | Runtime mempertahankan pipeline aslinya; core hanya menyimpan referensi/ringkasan koordinasi |
| D3 | Executor awal: **OpenCode + Hermes Agent** via adapter kontrak | Orca & DeepSeek harness: karantina evaluasi, bukan dependency |
| D4 | **D:\Obsidian** = pustaka pengetahuan utama | Knowledge store ≠ operational state; Librarian sebagai service |
| D5 | **Modular monolith** — satu control plane, batas modul jelas | Tanpa microservices/broker/vector-db sejak hari pertama |
| D6 | **SQLite** untuk state persisten awal; CLI antarmuka pertama | Migrasi PostgreSQL hanya jika banyak worker/banyak mesin terbukti perlu |
| D7 | **Event log + audit log sejak hari pertama** | Semua aksi Commander maupun agent terkecil melewati kontrol yang sama |
| D8 | **Organisasi sebagai data** (role/capability/workflow = konfigurasi tervalidasi) | Core bertipe kuat; definisi organisasi tidak perlu kompilasi ulang |
| D9 | Money-path trading **dikecualikan** dari migrasi sampai Phase 5+ | Sesuai PLAN.md: live-trading gate + regresi jurnal dulu; paper-trading only saat ini |
| D10 | PABS = **Labs** (domain penuh, bukan sub-divisi) | Katalog Labs, quant, engineering, media, business, community, personal, security |

## 3. Pemetaan aset sekarang → 12 domain arsitektur target

Fakta repo per 2026-10-30 (main `66f279a`, test 533 passed/28 skipped):

| # | Domain target | Status BlackHornet sekarang | Gap utama |
|---|---------------|------------------------------|-----------|
| 1 | Human Control & Experience | CLI launcher (`sovereign.py`, `integration/unified_launcher.py`); dev-server UI Next.js terpisah | Control room/dashboard terpadu, approval center |
| 2 | Identity & Governance | `AGENTS.md`, konstitusi `Dhaher_Labs_Constitution_v1.0.md` (dokumen) | Registry identitas/role/policy yang di-enforce kode |
| 3 | Intelligence & Planning | `src/colony/orchestrator.py`, omega engine | Planner berbasis goal registry + world state |
| 4 | Orchestration & Work Mgmt | ecosystem runner, immortal_daemon (shell-level), scheduler cron eksternal | **Task lifecycle state machine persisten**, dependency graph, retry/recovery |
| 5 | Federated Agent Runtimes | colony runtime sendiri; `hermes_quant.py`; bridge `colony_bridge.py` | **Adapter kontrak** untuk OpenCode/Hermes/eksternal |
| 6 | Capability & Organization Engine | `src/swarm/parked_agents.py`, roles sebagai prompt | Role/capability registry sebagai data; team composition |
| 7 | Tools & Access | `rust/crates/bh-mcp-bridge` (catalog + tool_bridge) — **seed MCP sudah ada** | MCP Center penuh, Credential Center, execution gateway, policy enforcement |
| 8 | Knowledge & Memory | `src/memory`, `memory_bridge.py`, MemoryFederation (FTS5/ChromaDB per PLAN.md) | Integrasi Obsidian + librarian + provenance + decision register |
| 9 | Assets & Environments | Git lokal + 5 remote fleet (sync guardian) | Repository registry metadata, worktree per task, workspace isolation |
| 10 | Domain Operations | trading (paper-only, omega), docs/00–17 | Workflow domain end-to-end per pack |
| 11 | Evaluation & Evolution | pytest 533/28 sebagai gerbang; guardian harian | Evaluation registry per jenis kerja, benchmark, promosi capability |
| 12 | Rust Core Infrastructure | `rust/crates/bh-rtd` (daemon/host/infinity adapter), `bh-bridge-uds` (IPC) | Durable state, event log, budget, recovery — subsistem penuh |

Plus 8 domain yang sering terlupakan (wajib ada di peta, tidak semua dibuat di M1): Model Gateway, Trigger & Event Engine, Notification & Approval Center, Artifact & Provenance, Budget & Resource Manager, Evaluation Registry, Backup & DR, Integration Lifecycle.

**Infra operasional luar repo** (supervisor v2-hardened, keepalive 10-mnt, watchdog 5-mnt, guardian harian, fleet 5 remote) = substrate operasional yang sudah membuktikan "durable + auto-recover" di level proses; nanti ditarik ke dalam platform sebagai lapisan observability/recovery.

## 4. Enam subsistem inti (Rust)

1. **Control Plane** — task graph, scheduler, dependency tracking, lifecycle, retry, recovery, event processing. *Seed: `bh-rtd` (daemon, timers durable).*
2. **Runtime Federation** — adapter kontrak: OpenCode, Hermes, colony runtime BlackHornet sendiri, executor generik CLI/API. *Seed: `bh-bridge-uds` + infinity adapter di `bh-rtd`.*
3. **Institution Engine** — organization/role/capability registry, team composition, domain packs, governance.
4. **Access & Resource Fabric** — MCP Center, Credential Center (referensi, bukan salinan rahasia), tool gateway, repo lokal/cloud adapter, permission enforcement. *Seed: `bh-mcp-bridge`.*
5. **Knowledge & State** — DB operasional, event log, project/world state, decision register, integrasi Obsidian (librarian).
6. **Trust & Evaluation** — authorization, sandbox, audit, benchmark, quality evaluation, budget enforcement, rollback, controlled upgrades.

Struktur workspace target (perluasan `rust/` yang sudah ada — bukan rewrite):

```
rust/
├── crates/            # sudah ada: bh-rtd, bh-mcp-bridge, bh-bridge-uds
│   ├── bh-domain/         # Task, Identity, Role, Policy, Capability (baru)
│   ├── bh-runtime/        # lifecycle, scheduling, cancellation (evolusi bh-rtd)
│   ├── bh-orchestrator/   # planning, delegation, dependencies (baru)
│   ├── bh-federation/     # adapter OpenCode, Hermes, colony, generik (baru)
│   ├── bh-policy/         # authorization, approvals, risk (baru)
│   ├── bh-storage/        # SQLite, migrations, event persistence (baru)
│   ├── bh-knowledge/      # memory interfaces, Obsidian/librarian (baru)
│   ├── bh-evaluation/     # tests, evidence, scoring (baru)
│   └── bh-cli/            # antarmuka pertama (baru)
├── packs/             # engineering, labs, quant, business, media-content,
│                      # personal-operations, knowledge, security, community
├── policies/  schemas/  migrations/  tests/  docs/
```

Pack Python BlackHornet (colony/omega/swarm) **tetap berjalan** dan menjadi salah satu runtime di federasi — tidak di-port.

## 5. Kontrak inti (wajib sebelum fitur)

- **Task lifecycle** — `Created → Planned → Running → Verifying → {Succeeded, Failed, Blocked, Canceled}`; setiap transisi tervalidasi + tercatat. Task berjalan tidak boleh dianggap sukses hanya karena agent berhenti menghasilkan output.
- **Agent contract** — identitas, capability list, schema I/O, batas izin, timeout, batas resource, aturan error.
- **Tool contract** — nama, skema input, level risiko, permission, timeout, hasil ter-audit. Semua tool dipanggil lewat satu execution gateway (tidak ada bypass).
- **Memory contract** — sumber, waktu, scope, jenis. Fakta observasi ≠ hipotesis agent ≠ keputusan manusia.
- **Loop organisme** — `Observe → Update World State → Detect Gaps → Plan → Delegate → Execute → Verify → Learn → Persist → Repeat`; berhenti saat goal tercapai / budget habis / blocked / butuh persetujuan.
- **Autonomi 4 mekanisme** — event-driven triggers; scheduled operations; goal-driven planning; controlled self-improvement (usul → eksperimen terisolasi → ukur → promosi/reject, dengan versi + rollback). Agent boleh mengusulkan perubahan pengaman, TIDAK boleh menonaktifkannya untuk menyetujui usulnya sendiri.

## 6. Data model inti

`Institution, Division, RoleDefinition, AgentInstance, RuntimeAdapter, Capability, WorkflowDefinition, Task, TaskDependency, Project, Resource, ToolDefinition, CredentialReference, KnowledgeRecord, DecisionRecord, Artifact, EvaluationRun, Policy, Event` — definisi ≠ instance (setiap eksekusi punya identitas, state, budget, audit trail sendiri).

## 7. Roadmap fase (B0–B8) — gate per fase

| Fase | Isi | Gate lulus |
|------|-----|------------|
| **B0** Konsolidasi + audit | Project Charter, ARCHITECTURE/SPEC, RUNTIME_CONTRACT, DOMAIN_CATALOG, SECURITY_MODEL, DECISIONS.md; audit faktual OpenCode & Hermes interface di mesin operator; inventaris repo lama | Spesifikasi tidak mencampur organisasi/runtime/capability/agent jadi satu konsep; hasil audit rebuild/reuse berbasis bukti |
| **B1** Control plane | Task model, state machine, SQLite, event log, scheduler, cancel/retry/timeout/recovery (`bh-domain`, `bh-storage`, evolusi `bh-rtd`) | Skenario sukses/crash-restart/timeout/retry/permission-denial lulus otomatis; task dipulihkan setelah restart |
| **B2** Satu alur nyata | 1 executor nyata (colony runtime BlackHornet via adapter) mengerjakan repo uji: konteks → eksekusi → test → artefak → laporan | Tidak ada keberhasilan palsu; hasil didukung bukti (build/test/diff) |
| **B3** Federasi | Adapter OpenCode + Hermes; capability-based executor selection; penanganan kegagalan konsisten | Orchestrator memilih executor by capability terverifikasi; internal runtime tidak digabung |
| **B4** Knowledge & Repo fabric | Obsidian integration, repository registry, project state, decision register, provenance | Sistem menemukan konteks tepat + melanjutkan pekerjaan tanpa seluruh riwayat chat |
| **B5** Organization & capability engine | Role/capability registry, team templates, domain packs, policy binding | Organisasi berbeda didefinisikan via konfigurasi tanpa runtime baru per role |
| **B6** Tool/MCP/Credential/Cloud | Execution gateway, konektor GitHub (5-remote fleet jadi kasus uji), credential references, approval workflow | Setiap tindakan: identitas + izin + audit + kill switch + recovery |
| **B7** Domain workflows | End-to-end: engineering/repo ops, Labs riset, quant riset (paper-only), content ops, librarian, personal assistant | Tiap domain menghasilkan artefak+bukti sesuai standar domainnya |
| **B8** Autonomous ops | Event triggers, scheduled work, goal tracking, meta-agent, capability development terukur | Sistem mengambil inisiatif dalam batas kebijakan, melaporkan alasan, berhenti aman, membuktikan manfaat vs baseline |

Urutan operasional: **"semuanya beres" dulu** — GL ship v7.0.0 tuntas dan operasi ekosistem stabil — baru B0 dimulai. B0 adalah kerja dokumen/kontrak yang bisa jalan paralel tanpa mengganggu operasi.

## 8. Acceptance criteria global (dipinjam utuh dari sesi visi)

Runtime federation (≥2 runtime via adapter, status+artefak terlacak) · Durable state (pulih pasca-restart) · Organization (role/tim dari konfigurasi) · Knowledge (konteks tepat + sumber) · Repository (workspace benar, diff reviewable) · Tool governance (aksi tanpa izin ditolak + tercatat) · Credential (rahasia tak pernah masuk prompt/log/dokumen) · Evaluation (dinilai kriteria domain, bukan klaim agent) · Autonomy (trigger/jadwal/anggaran/kondisi-berhenti berfungsi) · Evolution (capability baru diuji isolasi vs baseline lalu dipromosikan) · Recovery (kegagalan tanpa efek samping tak terdeteksi) · Observability (tujuan→delegasi→executor→tool→hasil→keputusan tertelusur).

## 9. Risiko utama & aturan proyek

**Risiko:** scope terlalu luas sebelum core jalan · terlalu banyak runtime · agent sprawl · memory sprawl · otonomi tanpa kontrol · **mengulang pola arsip lama**.

**Lima aturan (anti-pola kegagalan):**
1. Tidak ada fitur hanya karena terlihat keren.
2. Tidak ada ekspansi sebelum milestone lulus.
3. Tidak ada keberhasilan tanpa bukti (build/test/diff/laporan).
4. Tidak ada perubahan arsitektur diam-diam (semua keputusan dicatat).
5. Tidak ada penghapusan/rewrite impulsif — BlackHornet Python tetap hidup di setiap fase.

**Metrik sejak awal:** task selesai terverifikasi, tingkat kegagalan, intervensi manusia, biaya per hasil, waktu selesai, kualitas output, regresi pasca-perubahan. Target angka ditetapkan SETELAH baseline pengujian, bukan dikarang.

## 10. Keputusan terbuka

- Nama final platform (ATLAS = nama kerja; branding menyusul).
- Bentuk UI final (CLI dulu; command center kemudian).
- Vault credential konkret (referensi env var dulu; vault nyata menyusul).
- Prioritas domain pack setelah fondasi (engineering & Labs dulu; quant paper-only; media/community berikutnya).
- Hasil audit aktual OpenCode/Hermes (wajib sebelum B3).
