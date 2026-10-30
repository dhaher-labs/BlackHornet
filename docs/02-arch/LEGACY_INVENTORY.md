# Inventaris Repo Lama — BlackHornet (B0 Artifact)

- **Status:** Selesai · 2026-10-31 · commit awal slice ini
- **Sumber:** rekaman struktural repo GitLab lama `mulkymalikuldhr/blackhornet-deletion_scheduled-84098045` (project id `84098045`, `marked_for_deletion_at 2026-10-27`)
- **Metode:** clone read-only ke `/tmp` dengan token GitLab operator (token tidak pernah dicetak); tidak ada modifikasi pada repo lama; tidak ada konten file sensitif yang disalin — inventaris hanya metadata struktur (nama direktori, jumlah file, LOC, subjek commit).
- **Rujukan:** `docs/10-roadmap/ATLAS_PLAN.md` (baseline `66f279a` v7.0.0-105), `docs/00-vision/PROJECT_CHARTER.md` §6 (baris "Inventaris repo lama"), `docs/11-adr/DECISIONS.md` D1.
- **Batasan:** snapshot per 2026-10-31. Repo lama dijadwalkan terhapus, sehingga dokumen ini adalah rekaman struktural terakhir sebelum deletion. Angka LOC hanya menghitung `.py/.js/.ts` (diukur dengan `wc -l`, tidak termasuk `.git`).

---

## 1. Identitas repo lama

| Properti | Nilai |
|---|---|
| Platform | GitLab, project id `84098045` |
| Path final | `mulkymalikuldhr/blackhornet-deletion_scheduled-84098045` |
| Cabang utama | `main` |
| HEAD saat snapshot | `82fb90f` |
| Total commit | 162 |
| Commit pertama | `f7882d4` · 2026-05-24 · "v4.0.0 Production: Hermes Quant Operating System - Initial Release" |
| Commit terakhir | `82fb90f` · 2026-10-27 · "merge: reunify blackhornet mirror group — reconcile CB personal line into unified main" |
| Tag | `v1.0.0`, `v1.1.0`, `v2.0.0`, `v3.0.0`, `v3.1.0`, `v3.2.0`, `v4.0.0`, `v4.1.0` (8 tag; tidak ada tag ≥ v5) |
| Status | menunggu finalisasi deletion (ditandai 2026-10-27) |

Narasi sejarah (dari subjek commit): garis keturunan lama mulai sebagai **"Hermes Quant Operating System"** (v4.0.0, 2026-05-24), berevolusi menjadi **BlackHornet** (tag v1.0.0–v4.1.0, multi-agent swarm), kemudian mengintegrasikan **SAMAN-AI governance protocol + EcosystemState** (2026-10-08, 5 commit berturut), dan ditutup dengan reunifikasi mirror (Codeberg personal line → main) pada 2026-10-27 — tanggal yang sama dengan penandaan deletion.

## 2. Struktur top-level

~45 entri top-level, terbagi tiga kelompok:

1. **Dokumen root yang sangat banyak** (± 27 file Markdown di root): `AGENTS.md`, `SOUL.md`, `PLAN.md`, `PRODUCTION*.md` (3 file), `PROXYGATELLM_*AUDIT*.md` (2 file), `MULKY_REALISTIC_WEALTH_PLAN.md`, `WEALTH_REALITY_CHECK.md`, `HYPERGATE_MNEMOSYNE_STATUS.md`, `FINAL_REALIZATION.md`, `REALIZATION_REPORT.md`, `SESSION-SUMMARY.md`, `UPGRADE_PLAN.md`, `REFERENCES_V2_FULL.md`, `STRUCTURE.md`, `Dhaher_Labs_Constitution_v1.0.md`, `MASTER_SYSTEM_PROMPT.md`, README tiga bahasa (`README.md`, `README_id.md`, `README_zh.md`), dll.
2. **Kode**: `src/` (inti), `scripts/` (4.443 LOC), `skills/` (5.112 LOC), `tests/` (3.881 LOC), `ui/` (556 LOC), `swarm-config/` (181 LOC), plus file root (`sovereign.py`, dll. — 135 LOC).
3. **Infra**: `Dockerfile`, `docker-compose.yml`, `deploy/`, `deploy.sh`, `install.sh`, `hermes.sh`, `test.sh`, `e2b.json`, `e2b.toml`, `requirements.txt` (11 dependensi), `pyproject.toml`, `schemas/`, `data/`, `exports/`, `docs/`.

Deskripsi diri (README): *"BlackHornet — Autonomous Multi-Agent Swarm Framework: self-improving, immortal, autonomous AGI colony system implementing PSO ring topology, ABC role-switching, and stigmergic communication."*

## 3. Isi `src/` (142.245 LOC, 757 file .py + 2 file .js)

| Direktori | LOC | | Direktori | LOC |
|---|---|---|---|---|
| `harness/` | 36.383 | | `security/` | 1.619 |
| `agents/` | 24.262 | | `graph/` | 1.636 |
| `colony/` | 11.434 | | `channels/` | 1.407 |
| `memory/` | 9.258 | | `sandbox/` | 1.347 |
| `agent_tools/` | 9.248 | | `integration/` | 1.331 |
| `core/` | 8.868 | | `omega/` | 1.382 |
| `auto_discovery/` | 3.950 | | `integrations/` | 2.190 |
| `thinking/` | 3.357 | | `mcp/` | 2.178 |
| `tools/` | 4.909 | | `colony_types/` | 2.538 |
| `api/` | 2.691 | | `bh_types/` | 2.248 |
| `ui/` | 1.734 | | `swarm/` | 1.470 |
| `config/` | 625 | | `plugins/` | 285 |
| `browser/` | 672 | | `bridge/` | 232 |
| `hermes_nous/` | 146 | | `delta/` | 0 (kosong) |
| `llm_router/` | 0 (kosong) | | | |

Total seluruh repo (py/js/ts): **± 156.553 LOC**. Dua direktori kosong (`delta/`, `llm_router/`) menandakan rencana yang tidak pernah diisi.

## 4. Perbandingan dengan repo rebuild saat ini

Repo saat ini (clean rebuild, baseline `93f745e` = v7.0.0; ATLAS_PLAN baseline `66f279a` v7.0.0-105):

| Metrik | Repo lama | Rebuild v7.0.0 |
|---|---|---|
| `src/` LOC (py) | 142.245 (py/js/ts, 29 subdir) | 77.523 (py, 20 subdir + 9 modul root) |
| File `.py` di repo | 757 | 311 |
| Tests | 366 (subjek commit SAMAN, 2026-10-08) | 533 passed / 28 skipped (baseline berjalan) |
| Tag | v1.0.0–v4.1.0 | v7.0.0 |

Direktori `src/` yang **tidak dibawa** ke rebuild (direktori lama → LOC): `harness/` 36.383 · `api/` 2.691 · `integrations/` 2.190 · `mcp/` 2.178 · `channels/` 1.407 · `hermes_nous/` 146 · `bridge/` 232 · `delta/` 0 · `llm_router/` 0. Sisanya (20 direktori) dibawa dengan nama yang sama.

Catatan faktual (tanpa klaim baru):

- Modul terbesar repo lama, `src/harness/` (36.383 LOC — 25,6% dari `src/`), **tidak ikut** rebuild. Ini konsisten dengan `PROJECT_CHARTER.md` §2: *"HARNESS — satu lapisan kendali di atas semua coding agent/runtime. Rencana lama …"* — harness lama dianggap bagian dari masalah harness (§1 piagam) dan harness baru adalah pekerjaan B1+ dengan desain terpisah, bukan porting dari 36k LOC lama.
- `mcp/`, `integrations/`, `channels/`, `api/`, `bridge/` tidak dibawa sebagai direktori; fungsi serupa di desain baru didelegasikan ke seam stabil (`RUNTIME_CONTRACT.md` §Federation: MCP/RAP/ModelProvider, seed crate `bh-mcp-bridge`, `bh-bridge-uds`) — bukan salinan kode lama.
- Keputusan Rust-first (D1) menyebut Python BlackHornet "sudah terbukti untuk pipeline agent (533 tests)" — angka 533 adalah baseline rebuild, bukan 366 tests garis lama; keduanya adalah populasi test yang berbeda.
- History lama mengandung garis mirror lintas platform (merge GitHub/Codeberg/GitLab) dan periode commit otomatis (subjek `[auto] ...`, `... arden] self-upgrade`) — konteks mengapa clean rebuild dipilih sebagai dasar v7.0.0.

## 5. Untuk pekerjaan B1+

- **Referensi struktural**: bila perlu menelusuri perilaku lama (mis. psikologi swarm/PSO/stigmergy, skema memory, pola agent_tools), repo lama adalah sumber primer — tetapi sudah dijadwalkan terhapus. Snapshot faktual ada di dokumen ini; klon lokal `/tmp` bersifat sementara dan tidak di-commit.
- **Jangan porting mentah**: D1/D2 menetapkan federation runtime + seam stabil; kode lama hanya rujukan perilaku, bukan basis salin-tempel.
- **Harness B1+**: desain mulai dari piagam (§2) dan `ATLAS_PLAN.md` §7, dengan `harness/` lama (36.383 LOC) sebagai studi kasus anti-pola sekaligus referensi fitur.
- **Keamanan**: riwayat repo lama tidak diaudit untuk kredensial dalam dokumen ini (di luar lingkup B0); prinsip `SECURITY_MODEL.md` (rahasia tak pernah masuk prompt/log/dokumen) berlaku penuh — jangan menyalin isi file lama ke dokumen/diary/commit.

## 6. Pembaruan terkait

- `docs/00-vision/PROJECT_CHARTER.md` §6 — baris "Inventaris repo lama" diperbarui dari **Belum** → Selesai (commit ini).
- Setelah slice ini, artefak B0 tinggal 1: *Audit faktual OpenCode & Hermes interface* (butuh akses mesin operator; di luar jangkauan window cron).
