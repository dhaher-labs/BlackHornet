# RUNTIME_CONTRACT.md — Kontrak Inti Runtime (Artefak B0)

## Status
Accepted (2026-10-31) — bagian dari fase **B0 Konsolidasi + Audit** (ATLAS_PLAN.md §7).
Kontrak ini WAJIB dipenuhi sebelum fitur baru (ATLAS_PLAN.md §5: "Kontrak inti
(wajib sebelum fitur)").

## Sumber
Diekstrak verbatim dari `docs/10-roadmap/ATLAS_PLAN.md` §4–§6, `PLAN.md` (posture dan
stable seams), dan register `docs/11-adr/DECISIONS.md`. Kontrak ini menghormati D1
(Rust-first core), D2 (federasi runtime), D3 (OpenCode + Hermes via adapter kontrak),
dan D7 (event log + audit log sejak hari pertama). Tanpa klaim baru di luar sumber itu.

## Batasan lapisan (gate B0)
Kontrak ini HANYA mengatur **lapisan runtime**. Sesuai gate B0 dan
`docs/02-arch/DOMAIN_CATALOG.md`, dokumen ini tidak mendefinisikan:
- struktur organisasi (domain/role/workflow → DOMAIN_CATALOG + Institution Engine);
- katalog capability bisnis (bagian dari organisasi sebagai data, D8);
- pengetahuan utama operator (Obsidian/pustaka → D4, bukan operational state).
Yang diatur di sini: task lifecycle, agent contract, tool contract, memory contract,
loop organisme, autonomi, federasi runtime, dan event/audit. **Definisi ≠ instance**
(ATLAS_PLAN.md §6): setiap eksekusi punya identitas, state, budget, audit trail sendiri.

## 1. Task lifecycle
`Created → Planned → Running → Verifying → {Succeeded, Failed, Blocked, Canceled}`

- Setiap transisi **tervalidasi** (transisi ilegal ditolak) dan **tercatat** (event log + audit log, D7).
- Task berjalan **tidak boleh dianggap sukses hanya karena agent berhenti menghasilkan
  output** — status `Succeeded` hanya lewat tahap `Verifying`.
- Terminal state: `Succeeded`, `Failed`, `Blocked`, `Canceled`. Transisi keluar dari
  terminal state dilarang; retry/recovery = task instance baru dengan referensi ke task lama.

## 2. Agent contract
Setiap agent instance mendeklarasikan (ATLAS_PLAN.md §5):
- **identitas** (definition ≠ instance: `AgentInstance` ≠ `RoleDefinition`);
- **capability list** (apa yang boleh dilakukan);
- **schema I/O** (input dan output bertipe, tervalidasi);
- **batas izin** (permission enforcement lewat Access & Resource Fabric, bukan honor system);
- **timeout** dan **batas resource** (budget enforcement di Trust & Evaluation);
- **aturan error** (apa yang dilakukan saat gagal: fail-fast, retry, atau escalate/blocked).

## 3. Tool contract
Setiap tool mendeklarasikan: **nama, skema input, level risiko, permission, timeout,
hasil ter-audit**.
- Semua tool dipanggil **lewat satu execution gateway** — tidak ada bypass.
- Seed implementasi: `bh-mcp-bridge` (Access & Resource Fabric, ATLAS_PLAN.md §4.4).
- Credential hanya diakses sebagai **referensi, bukan salinan rahasia**
  (`CredentialReference`, ATLAS_PLAN.md §6).

## 4. Memory contract
Setiap record memori membawa: **sumber, waktu, scope, jenis**.
- **Fakta observasi ≠ hipotesis agent ≠ keputusan manusia** — jenis record dipisahkan
  dan tidak boleh saling menimpa.
- Operational state (DB, event log, project/world state, decision register) dipisahkan
  dari pustaka pengetahuan (Obsidian via librarian service, D4) — sesuai pemisahan
  lapisan Knowledge & State (ATLAS_PLAN.md §4.5).

## 5. Loop organisme
`Observe → Update World State → Detect Gaps → Plan → Delegate → Execute → Verify →
Learn → Persist → Repeat`

Loop berhenti hanya saat: **goal tercapai / budget habis / blocked / butuh persetujuan**.

## 6. Autonomi — 4 mekanisme (ATLAS_PLAN.md §5)
1. **Event-driven triggers**
2. **Scheduled operations**
3. **Goal-driven planning**
4. **Controlled self-improvement**: usul → eksperimen terisolasi → ukur →
   promosi/reject, dengan **versi + rollback**.

**Guardrail terkunci**: agent boleh **mengusulkan** perubahan pengaman, TIDAK boleh
**menonaktifkannya** untuk menyetujui usulnya sendiri ( approvals tetap jalur manusia/policy).

## 7. Federasi runtime (D1 + D2 + D3)
- Satu **orchestrator** di atas **banyak runtime**; runtime mempertahankan pipeline
  aslinya — core hanya menyimpan **referensi/ringkasan koordinasi** (D2).
- Executor awal: **OpenCode + Hermes Agent** via **adapter kontrak** (D3); Orca &
  DeepSeek harness di**karantina** evaluasi, bukan dependency.
- Python BlackHornet (colony/omega/swarm) **tetap berjalan** sebagai salah satu runtime
  di federasi — tidak di-port (posture PLAN.md: "adopt the protocol, embed the runtime,
  never port the colony").
- Flag `BH_RUNTIME=python|rust`, **default `python`**; money-path trading dikecualikan
  dari migrasi sampai Phase 5+ (D9).
- Stable seams yang dijaga: **MCP, RAP, `ModelProvider`, store traits** (PLAN.md).
- Seed implementasi (ATLAS_PLAN.md §4): `bh-rtd` (control plane, durable timers),
  `bh-bridge-uds` + infinity adapter (Runtime Federation), `bh-mcp-bridge` (Access &
  Resource Fabric). Crate `bh-federation` = adapter OpenCode/Hermes/colony/generik (baru,
  evolusi — bukan rewrite).

## 8. Event & audit (D7)
- **Event log + audit log sejak hari pertama** — semua aksi Commander maupun agent
  terkecil melewati kontrol yang sama.
- Yang wajib tercatat: setiap transisi task lifecycle (§1), setiap pemanggilan tool
  beserta hasilnya (§3), setiap keputusan promosi/reject self-improvement (§6).

## 9. Acceptance check B0 (ATLAS_PLAN.md §7 gate)
- [x] Kontrak tidak mencampur organisasi/runtime/capability/agent jadi satu konsep
      (batasan lapisan di atas; organisasi → DOMAIN_CATALOG/DECISIONS.md D8).
- [x] Definisi ≠ instance dipertahankan di seluruh kontrak (§2, ATLAS_PLAN.md §6).
- [x] Semua 6 kontrak §5 ATLAS_PLAN hadir tanpa perubahan semantik (§1–§6 di atas).
- [x] Federasi runtime konsisten dengan D1/D2/D3 dan posture PLAN.md (§7).
- [x] Event/audit day-one konsisten dengan D7 (§8).

## Referensi silang
- `docs/10-roadmap/ATLAS_PLAN.md` §4–§7 — sumber kontrak, subsistem, gate fase.
- `docs/11-adr/DECISIONS.md` D1, D2, D3, D7 (dan D9 untuk pengecualian money-path).
- `docs/02-arch/DOMAIN_CATALOG.md` — pemisahan 3 lapisan domain/arsitektur/runtime.
- `PLAN.md` — posture strangler-fig, stable seams, fase runtime wedge.
