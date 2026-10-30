# DECISIONS.md — Register Keputusan Terkunci (Artefak B0)

## Status
Accepted (2026-10-31) — bagian dari fase **B0 Konsolidasi + Audit** (ATLAS_PLAN.md §7).

## Sumber
Register ini memformalkan 10 keputusan terkunci operator dari sesi visi 28 Oktober 2026
(yang terdokumentasi di `docs/10-roadmap/ATLAS_PLAN.md` §2). Keputusan berlaku sebagai
kendaraan pengarah desain fase B0–B8; perubahan atau pembatalan satu keputusan HANYA
melalui ADR baru di direktori ini yang merujuk ID keputusan terkait.

## Konvensi
- **Status**: `Accepted` (terkunci) atau `Superseded by ADR-xxx` (dicabut via ADR).
- Setiap entri: Konteks → Keputusan → Konsekuensi. Tidak ada keputusan tanpa konsekuensi.
- Keputusan ini membatasi desain: spesifikasi B0 berikutnya (RUNTIME_CONTRACT,
  ARCHITECTURE/SPEC, DOMAIN_CATALOG) tidak boleh melanggar entri mana pun di sini.

## Ringkasan register

| ID | Keputusan | Status | Tanggal |
|----|-----------|--------|---------|
| D1 | Rust-first untuk core control plane | Accepted | 2026-10-28 |
| D2 | Federasi runtime — satu orchestrator di atas banyak runtime | Accepted | 2026-10-28 |
| D3 | Executor awal: OpenCode + Hermes Agent via adapter kontrak | Accepted | 2026-10-28 |
| D4 | Obsidian = pustaka pengetahuan utama | Accepted | 2026-10-28 |
| D5 | Modular monolith — satu control plane, batas modul jelas | Accepted | 2026-10-28 |
| D6 | SQLite untuk state persisten awal; CLI antarmuka pertama | Accepted | 2026-10-28 |
| D7 | Event log + audit log sejak hari pertama | Accepted | 2026-10-28 |
| D8 | Organisasi sebagai data | Accepted | 2026-10-28 |
| D9 | Money-path trading dikecualikan dari migrasi sampai Phase 5+ | Accepted | 2026-10-28 |
| D10 | PABS = Labs (domain penuh, bukan sub-divisi) | Accepted | 2026-10-28 |

## Detail keputusan

### D1 — Rust-first untuk core control plane
- **Konteks**: Core control plane butuh durable state, event processing, dan recovery yang
  dapat diandalkan jangka panjang; Python BlackHornet sudah terbukti untuk pipeline agent
  (533 tests) tetapi bukan basis ideal untuk daemon state yang harus tidak pernah hilang.
- **Keputusan**: Core control plane dibangun Rust-first, sebagai perluasan `rust/crates/`
  yang sudah ada (`bh-rtd`, `bh-mcp-bridge`, `bh-bridge-uds`) — bukan rewrite.
- **Konsekuensi**: Python BlackHornet tetap hidup dan tetap runtime utama saat ini sesuai
  posture `PLAN.md` (`BH_RUNTIME=python|rust`, default `python`); kode Rust baru masuk
  sebagai crate terpisah dengan gate sendiri; interop via kontrak (UDS/adapter), bukan FFI
  sembarangan.

### D2 — Federasi runtime: satu orchestrator di atas banyak runtime
- **Konteks**: Sistem harus mampu memakai lebih dari satu eksekutor agent (internal maupun
  eksternal) tanpa menyeragamkan internal runtime mereka.
- **Keputusan**: Satu orchestrator mengoordinasikan banyak runtime; runtime mempertahankan
  pipeline aslinya.
- **Konsekuensi**: Core hanya menyimpan referensi/ringkasan koordinasi — bukan replika
  state internal runtime; setiap runtime di-balik adapter kontrak (lihat D3); kegagalan
  satu runtime tidak boleh merusak state orchestrator.

### D3 — Executor awal: OpenCode + Hermes Agent via adapter kontrak
- **Konteks**: Fase B3 butuh minimal dua executor eksternal nyata untuk membuktikan
  federasi; pemilihan terlambat menunda validasi adapter.
- **Keputusan**: Executor awal adalah OpenCode dan Hermes Agent, diintegrasikan lewat
  adapter kontrak (bukan fork/patch internal mereka).
- **Konsekuensi**: Orca dan DeepSeek harness diletakkan di karantina evaluasi — mereka
  bukan dependency; adapter adalah permukaan integrasi satu-satunya yang didukung; kriteria
  lulus adapter didefinisikan sebelum implementasi (spec-first).

### D4 — Obsidian sebagai pustaka pengetahuan utama
- **Konteks**: Pengetahuan lintas-proyek butuh satu pustaka yang dapat dibaca manusia dan
  mesin sekaligus.
- **Keputusan**: Obsidian (`D:\Obsidian`) adalah pustaka pengetahuan utama.
- **Konsekuensi**: Knowledge store ≠ operational state (state operasional tetap di DB,
  lihat D6); Librarian dijalankan sebagai service; penulisan ke Obsidian melewati interface
  librarian, tidak langsung dari agent mana pun.

### D5 — Modular monolith
- **Konteks**: Microservices, broker pesan, dan vector-db sejak hari pertama menambah
  biaya operasional tanpa bukti kebutuhan skala.
- **Keputusan**: Satu control plane dengan batas modul yang jelas (modular monolith).
- **Konsekuensi**: Tidak ada microservices/broker/vector-db sejak hari pertama; batas modul
  di-enforce di tipe dan struktur crate; pemecahan menjadi service hanya jika beban
  terukur menuntutnya.

### D6 — SQLite untuk state persisten awal; CLI antarmuka pertama
- **Konteks**: State harus durable pasca-restart sejak B1; antarmuka paling murah untuk
  memvalidasi kontrak adalah CLI.
- **Keputusan**: SQLite sebagai state persisten awal; CLI sebagai antarmuka pertama.
- **Konsekuensi**: Migrasi ke PostgreSQL hanya jika banyak worker/banyak mesin terbukti
  perlu; semua interaksi kontrol plane harus bisa dilakukan via CLI sebelum UI dibangun;
  skema SQLite di-migrate terkontrol (bukan ad-hoc).

### D7 — Event log + audit log sejak hari pertama
- **Konteks**: Akuntabilitas penuh (tujuan → delegasi → executor → tool → hasil →
  keputusan) tidak bisa ditambal belakangan jika tidak dicatat dari awal.
- **Keputusan**: Event log dan audit log diaktifkan sejak hari pertama.
- **Konsekuensi**: Semua aksi Commander maupun agent terkecil melewati kontrol yang sama;
  aksi tanpa jejak event/audit dianggap gagal (acceptance criteria global); log adalah
  bagian dari durable state (D6), bukan stdout yang hilang.

### D8 — Organisasi sebagai data
- **Konteks**: Role, capability, dan tim yang di-hardcode membuat organisasi baru butuh
  deploy ulang.
- **Keputusan**: Organisasi (role/capability/workflow) didefinisikan sebagai konfigurasi
  tervalidasi — "organisasi sebagai data".
- **Konsekuensi**: Core bertipe kuat; definisi organisasi tidak perlu kompilasi ulang;
  validasi definisi dilakukan saat load (schema + referensi), definisi invalid ditolak
  sebelum dipakai.

### D9 — Money-path trading dikecualikan dari migrasi sampai Phase 5+
- **Konteks**: Jalur uang nyata berisiko tinggi dan tidak boleh terpengaruh migrasi
  arsitektur; pengujian dan auditnya belum lengkap.
- **Keputusan**: Money-path trading dikecualikan dari migrasi sampai Phase 5+.
- **Konsekuensi**: Sesuai `PLAN.md`: live-trading gate + regresi jurnal terlebih dahulu;
  posture saat ini paper-trading only; komponen trading yang dimigrasi hanya yang non-money
  (riset, jurnal, evaluasi).

### D10 — PABS = Labs (domain penuh, bukan sub-divisi)
- **Konteks**: PABS sebelumnya diposisikan ambigu sebagai sub-divisi; itu merusak model
  organisasi (D8) dan katalog domain.
- **Keputusan**: PABS menjadi domain penuh bernama Labs.
- **Konsekuensi**: Katalog domain: Labs, quant, engineering, media, business, community,
  personal, security; Labs punya workflow, standar artefak, dan evaluasi sendiri sebagaimana
  domain lain.

## Hubungan dengan artefak B0 lain
- RUNTIME_CONTRACT (berikutnya) harus menghormati D1, D2, D3, D7.
- DOMAIN_CATALOG harus menghormati D8 dan D10.
- SECURITY_MODEL harus menghormati D7 dan D9.
- DECISIONS.md ini adalah sumber kebenaran keputusan; ATLAS_PLAN.md §2 tetap sebagai
  rekaman asli sesi visi.
