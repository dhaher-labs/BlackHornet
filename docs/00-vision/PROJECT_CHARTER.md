# PROJECT_CHARTER.md — Piagam Proyek BlackHornet → ATLAS (Artefak B0)

## Status
Accepted (2026-10-31) — bagian dari fase **B0 Konsolidasi + Audit** (ATLAS_PLAN.md §7).
Piagam ini adalah titik masuk tunggal proyek: menautkan visi, keputusan terkunci,
artefak spesifikasi, dan gate — tanpa menggantikan dokumen sumber mana pun.

## Sumber
`docs/10-roadmap/ATLAS_PLAN.md` (rencana ulang v1.0, baseline 2026-10-30 @ `66f279a`,
sumber visi: sesi strategi operator 28 Okt 2026), `PLAN.md` (ship v7.0.0 +
strangler-fig Rust), `ROADMAP.md` (snapshot v7.0.0), `docs/00-vision/VISION.md`,
`docs/11-adr/DECISIONS.md`, serta artefak B0 lain. Tanpa klaim baru di luar sumber itu.

## 1. Identitas proyek
- **Nama kerja**: ATLAS (nama final = keputusan terbuka, ATLAS_PLAN §10).
- **Baseline**: 2026-10-30, repo @ `66f279a` (v7.0.0-105), status RENCANA — belum
  diimplementasikan.
- **Relasi**: melengkapi PLAN.md dan ROADMAP.md; tidak menggantikan keduanya.

## 2. Masalah inti (ATLAS_PLAN §1)
Operator tidak kekurangan agent, prompt, atau ide divisi. Yang kurang adalah
**HARNESS** — satu lapisan kendali di atas semua coding agent/runtime. Rencana lama
(BlackHornet v1–v6, AMC, Autonomous Organism) diarsipkan karena yang dibangun adalah
organisasi/persona, bukan lapisan koordinasi yang bisa mengelola pekerjaan dari awal
sampai selesai.

## 3. Arah (ATLAS_PLAN §1)
**Platform institusi AI** — satu control plane yang dapat mengoperasikan banyak
organisasi (Personal OS → Project OS → Institution OS), menggunakan banyak runtime
(federasi), membentuk tim dari registry capability, dan mengembangkan kemampuan lewat
bukti terukur.

Tiga pemisahan yang wajib dijaga:
1. **Orchestrator bukan agent super** — mengatur task/dependensi/verifikasi.
2. **Runtime bukan divisi** — Hermes bisa dipakai banyak divisi; tidak ada runtime
   baru per jabatan.
3. **Organisasi bukan workflow kaku** — template + kebijakan; tim disusun per tujuan,
   di dalam batas izin/biaya/keselamatan.

## 4. Ruang lingkup
**Di dalam scope**: control plane Rust-first (D1), federasi runtime (D2), organisasi
sebagai data (D8), knowledge/state terpisah dari pustaka (D4), tool governance +
credential reference (§4.4), trust & evaluation (§4.6), CLI sebagai antarmuka pertama
(D6).

**Di luar scope (sekarang)**: money-path trading sampai Phase 5+ (D9); UI command
center (CLI dulu — keputusan terbuka §10); vault credential konkret (referensi env var
dulu — §10); runtime baru per jabatan (dilarang — pemisahan #2); Orca & DeepSeek
sebagai dependency (karantina evaluasi — D3).

## 5. Keputusan terkunci
10 keputusan operator (D1–D10, sesi 28 Okt 2026) berlaku sebagai kendaraan pengarah
desain fase B0–B8: register penuh + konsekuensi ada di `docs/11-adr/DECISIONS.md`.
Ringkas: D1 Rust-first core · D2 federasi runtime · D3 OpenCode+Hermes via adapter ·
D4 Obsidian pustaka utama · D5 modular monolith · D6 SQLite+CLI-first · D7 event+audit
day-one · D8 organisasi sebagai data · D9 money-path excluded Phase 5+ · D10 PABS =
Labs. Perubahan HANYA via ADR baru.

## 6. Artefak B0 (ATLAS_PLAN §7 — "Konsolidasi + audit")
| Artefak | Lokasi | Status |
|---|---|---|
| DECISIONS.md (register D1–D10) | `docs/11-adr/DECISIONS.md` | Selesai @ `80742be` |
| DOMAIN_CATALOG.md (3 lapisan + 12 domain) | `docs/02-arch/DOMAIN_CATALOG.md` | Selesai @ `c27f7df` |
| RUNTIME_CONTRACT.md (6 kontrak inti) | `docs/03-spec/RUNTIME_CONTRACT.md` | Selesai @ `1ecdd68` |
| SECURITY_MODEL.md (D7/D9) | `docs/07-security/SECURITY_MODEL.md` | Selesai @ `1a490f7` |
| PROJECT_CHARTER.md (piagam ini) | `docs/00-vision/PROJECT_CHARTER.md` | Selesai (piagam) |
| Audit faktual OpenCode & Hermes interface | mesin operator | **Belum** |
| Inventaris repo lama | — | **Belum** |

Gate B0: spesifikasi tidak mencampur organisasi/runtime/capability/agent jadi satu
konsep; hasil audit rebuild/reuse berbasis bukti.

## 7. Roadmap fase (ringkas — gate penuh di ATLAS_PLAN §7)
B0 Konsolidasi+audit → B1 Control plane (task model, state machine, SQLite, event log;
skenario crash-restart/timeout/retry/permission-denial lulus otomatis) → B2+ … →
gate per fase di ATLAS_PLAN.md §7. Python BlackHornet tetap hidup di setiap fase
(aturan #5 §9).

## 8. Acceptance criteria global (ATLAS_PLAN §8 — verbatim)
Runtime federation (≥2 runtime via adapter, status+artefak terlacak) · Durable state
(pulih pasca-restart) · Organization (role/tim dari konfigurasi) · Knowledge (konteks
tepat + sumber) · Repository (workspace benar, diff reviewable) · Tool governance
(aksi tanpa izin ditolak + tercatat) · Credential (rahasia tak pernah masuk prompt/log/
dokumen) · Evaluation (dinilai kriteria domain, bukan klaim agent) · Autonomy
(trigger/jadwal/anggaran/kondisi-berhenti berfungsi) · Evolution (capability baru
diuji isolasi vs baseline lalu dipromosikan) · Recovery (kegagalan tanpa efek samping
tak terdeteksi) · Observability (tujuan→delegasi→executor→tool→hasil→keputusan
tertelusur).

## 9. Lima aturan anti-kegagalan (ATLAS_PLAN §9 — verbatim)
1. Tidak ada fitur hanya karena terlihat keren.
2. Tidak ada ekspansi sebelum milestone lulus.
3. Tidak ada keberhasilan tanpa bukti (build/test/diff/laporan).
4. Tidak ada perubahan arsitektur diam-diam (semua keputusan dicatat).
5. Tidak ada penghapusan/rewrite impulsif — BlackHornet Python tetap hidup di setiap fase.

## 10. Metrik sejak awal (ATLAS_PLAN §9)
Task selesai terverifikasi, tingkat kegagalan, intervensi manusia, biaya per hasil,
waktu selesai, kualitas output, regresi pasca-perubahan. **Target angka ditetapkan
SETELAH baseline pengujian, bukan dikarang.**

## 11. Keputusan terbuka (ATLAS_PLAN §10)
Nama final platform (ATLAS = nama kerja) · bentuk UI final (CLI dulu) · vault
credential konkret · prioritas domain pack setelah fondasi (engineering & Labs dulu;
quant paper-only; media/community berikutnya).

## Referensi silang
- `docs/10-roadmap/ATLAS_PLAN.md` — rencana ulang penuh (§1 visi, §7 gate, §8-§10).
- `PLAN.md` / `ROADMAP.md` — posture ship v7.0.0 dan strangler-fig.
- `docs/11-adr/DECISIONS.md` — register D1–D10.
- `docs/02-arch/DOMAIN_CATALOG.md` · `docs/03-spec/RUNTIME_CONTRACT.md` ·
  `docs/07-security/SECURITY_MODEL.md` — artefak B0 terkait.
- `docs/00-vision/VISION.md` — visi eksisting v7.0.0 (nilai dan misi jangka panjang).
