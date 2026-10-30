# SECURITY_MODEL.md — Model Keamanan (Artefak B0)

## Status
Accepted (2026-10-31) — bagian dari fase **B0 Konsolidasi + Audit** (ATLAS_PLAN.md §7).
Model ini menghormati **D7** (event log + audit log sejak hari pertama) dan **D9**
(money-path trading dikecualikan sampai Phase 5+) sesuai register
`docs/11-adr/DECISIONS.md`.

## Sumber
Diekstrak verbatim dari: `docs/10-roadmap/ATLAS_PLAN.md` §2 (D7, D9), §4.4 (Access &
Resource Fabric), §4.6 (Trust & Evaluation), §5 (tool contract + guardrail autonomi),
§6 (data model), §8 (acceptance criteria), §10 (keputusan terbuka), §7 (gate B1/B6);
`docs/07-security/SECURITY.md` (prinsip eksisting); `docs/03-spec/RUNTIME_CONTRACT.md`
(§3 tool gateway, §6 guardrail, §8 event/audit); PLAN.md (live-trading gate). Tanpa
klaim baru di luar sumber itu.

## Batasan lapisan (gate B0)
Dokumen ini HANYA model keamanan. Pola kerja runtime → `RUNTIME_CONTRACT.md`;
struktur organisasi & domain → `DOMAIN_CATALOG.md`; register keputusan →
`DECISIONS.md`. Model keamanan berlaku **sama untuk semua aktor**: Commander maupun
agent terkecil melewati kontrol yang sama (D7).

## 1. Prinsip (warisan docs/07-security/SECURITY.md + ATLAS_PLAN §8)
1. **No secrets in code** — rahasia hanya via environment variables / `.env` (gitignored).
2. **Input validation di setiap boundary.**
3. **Auth untuk setiap panggilan API eksternal.**
4. **Dependency scanning** untuk kerentanan.
5. **Rahasia tak pernah masuk prompt/log/dokumen** (ATLAS_PLAN §8, kriteria Credential).
6. **Aksi tanpa izin ditolak + tercatat** (ATLAS_PLAN §8, kriteria Tool governance).

## 2. Identitas & otorisasi (Trust & Evaluation, ATLAS_PLAN §4.6)
- Setiap tindakan membawa **identitas + izin + audit + kill switch + recovery**
  (gate B6, ATLAS_PLAN §7).
- Otorisasi diimplementasi di **Access & Resource Fabric** (§4.4): permission
  enforcement via `bh-mcp-bridge` (seed), crate `bh-policy` (authorization, approvals,
  risk — baru, ATLAS_PLAN §4).
- `AgentInstance` membawa batas izin sendiri (definisi ≠ instance, ATLAS_PLAN §6);
  capability list agent ≠ hak akses — hak dievaluasi per tindakan.

## 3. Credential management (ATLAS_PLAN §4.4 + §6 + §10; SECURITY.md)
- Credential diakses sebagai **referensi, bukan salinan rahasia** (`CredentialReference`).
- **Vault credential konkret menyusul** — sekarang referensi env var
  (keputusan terbuka ATLAS_PLAN §10); token per service, scope minimal (SECURITY.md).
- Rahasia **tidak pernah** masuk prompt, log, atau dokumen (ATLAS_PLAN §8).
- `.env.*` gitignored; tidak pernah commit API key, token, password, private key
  (SECURITY.md).

## 4. Tool governance (RUNTIME_CONTRACT §3 + ATLAS_PLAN §5 + gate B6)
- Semua tool dipanggil **lewat satu execution gateway — tidak ada bypass**
  (ATLAS_PLAN §5; RUNTIME_CONTRACT §3).
- Setiap tool deklarasikan **level risiko + permission + timeout**; hasil
  **ter-audit**.
- Tool di Catalog via `ToolDefinition`; pemanggilan tanpa izin **ditolak dan
  tercatat** (ATLAS_PLAN §8).

## 5. Sandbox & isolasi (ATLAS_PLAN §4.6 + RUNTIME_CONTRACT §6)
- Controlled self-improvement: **usul → eksperimen terisolasi → ukur →
  promosi/reject, dengan versi + rollback** (ATLAS_PLAN §5; RUNTIME_CONTRACT §6).
- Test dijalankan di environment terisolasi; repo eksternal akses **read-only via
  token** (SECURITY.md).

## 6. Audit & event day-one (D7)
- **Event log + audit log sejak hari pertama** — semua aksi Commander maupun agent
  terkecil melewati kontrol yang sama (D7; RUNTIME_CONTRACT §8).
- Wajib tercatat: transisi task lifecycle, pemanggilan tool + hasilnya, keputusan
  promosi/reject self-improvement (RUNTIME_CONTRACT §8).
- `DecisionRecord` dan `Event` adalah entitas data model inti (ATLAS_PLAN §6).

## 7. Money-path guard (D9 + PLAN.md)
- Live-trading **dikecualikan** dari migrasi sampai Phase 5+; paper-trading only
  saat ini (D9).
- Flip `BH_RUNTIME` untuk money-path butuh **live-trading gate + regresi semantik
  jurnal** yang sudah lulus (PLAN.md posture).

## 8. Guardrail autonomi (ATLAS_PLAN §5; RUNTIME_CONTRACT §6)
- Agent boleh **mengusulkan** perubahan pengaman; **TIDAK boleh menonaktifkannya**
  untuk menyetujui usulnya sendiri — approval tetap jalur manusia/policy.
- Loop berhenti saat **budget habis**; batas resource per `AgentInstance`
  di-enforce (Trust & Evaluation: budget enforcement).

## 9. Acceptance check B0 (ATLAS_PLAN.md §7 gate)
- [x] Model menghormati D7 (audit day-one, §6) dan D9 (money-path guard, §7).
- [x] Tidak mencampur lapisan: keamanan terpisah dari pola runtime dan organisasi.
- [x] Konsisten dengan RUNTIME_CONTRACT (satu execution gateway, guardrail,
      event/audit) — tanpa duplikasi definisi.
- [x] Kriteria ATLAS_PLAN §8 terwakali: credential, tool governance, autonomy,
      recovery, observability (identitas→keputusan tertelusur).
- [x] Keputusan terbuka (vault konkret) tercatat sebagai penyusul, bukan klaim.

## Referensi silang
- `docs/10-roadmap/ATLAS_PLAN.md` §2, §4, §5, §6, §7, §8, §10 — sumber utama.
- `docs/11-adr/DECISIONS.md` D7, D9 (dan D5/D6 untuk konteks monolith/SQLite).
- `docs/03-spec/RUNTIME_CONTRACT.md` §3, §6, §8 — kontrak yang diamankan model ini.
- `docs/07-security/SECURITY.md` — prinsip keamanan eksisting v7.0.0.
- `PLAN.md` — posture live-trading gate dan strangler-fig.
