# Changelog

## 0.1.0 — 2026-07-15

Initial public release.

- Daemon watches PSI (`/proc/pressure/memory`), `MemAvailable` and swap IO,
  driving `vm.swappiness` / `vm.vfs_cache_pressure` through a hysteretic
  Calm → Pressured → Critical policy with a ThrashGuard backoff state.
- Tiered swap management: ZRAM (lz4, priority 200) → flash device
  (priority 100) → disk fallback (priority 10), set up idempotently.
- Watchdog pins swappiness when the flash tier disappears and re-arms on
  return.
- CLI: `daemon`, `setup [--format]`, `teardown`, `status`.
- Zero external dependencies; Rust 2024 edition.
