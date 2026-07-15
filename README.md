# Umami

A multi-tier memory pressure buffering system for Linux. It virtualises
**capacity — not speed** — by absorbing memory pressure into managed,
flash-backed swap tiers before kernel collapse (thrash → freeze → OOM)
ever occurs.

Umami is not RAM. It cannot add addressable memory over USB — no userspace
software can. What it does is reshape paging behaviour so the system
degrades *deterministically* instead of chaotically:

```
RAM → ZRAM (compressed) → Umami flash tier (USB/NVMe) → disk swap (last resort)
```

This is the same trick ReadyBoost pulled off: not RAM, but a
cache + swap accelerator that keeps hot working sets in physical memory.

## How it works

The `umami` daemon samples kernel pressure signals once per second:

- `/proc/pressure/memory` — PSI `some`/`full` averages
- `/proc/meminfo` — `MemAvailable`
- `/proc/vmstat` — swap-in/out rates

and drives a hysteretic state machine:

| State        | Trigger                                   | swappiness | cache_pressure |
| ------------ | ----------------------------------------- | ---------- | -------------- |
| Calm         | quiet system                              | 60         | 100            |
| Pressured    | PSI some avg10 ≥ 5, or MemAvailable ≤ 15% | 120        | 50             |
| Critical     | PSI some avg10 ≥ 20, or MemAvailable ≤ 5% | 180        | 50             |
| ThrashGuard  | swap-in ≥ 50 MiB/s with full stalls       | 30 (backoff) | 50           |

Escalation needs 2 consecutive demanding samples; relaxation needs 10
quiet ones — spikes don't move the system, sustained pressure does.

**ThrashGuard** is the distinctive part: when the flash tier itself
saturates (high swap-in rates *and* full memory stalls), Umami stops
feeding it by dropping swappiness, rather than compounding the pileup.

**Watchdog**: if the flash device disappears (USB pulled), swappiness is
pinned to `watchdog_swappiness` (10) so no new pages are pushed toward
the dead tier, and the tier is re-armed automatically when it returns.

## Tier hardware guidance

- **Flash tier**: USB 3.1+ stick or, far better, an external NVMe
  enclosure. You care about *random* I/O and wear leveling, not
  sequential benchmarks. Format `f2fs` or plain swap; mount data
  filesystems with `noatime` if you use one.
- **ZRAM first**: the default 8 GiB lz4 ZRAM tier at priority 200
  compresses cold pages ~1.5–2× in RAM and absorbs most pressure before
  flash sees a single write. This is your flash wear protection.
- **Disk swap**: priority 10 — strictly a last resort.

| Tier             | Latency   | Notes                   |
| ---------------- | --------- | ----------------------- |
| RAM              | ~100 ns   | hot working set only    |
| ZRAM             | ~1–5 µs   | CPU tradeoff            |
| Umami (USB NVMe) | ~50–150 µs| acceptable for cold pages|
| Disk swap        | ~ms       | avoid                   |

Umami works because most pressure comes from *cold but large*
allocations; those pages are rarely faulted again, so tier latency only
matters on re-entry.

## Install

```sh
./scripts/install.sh          # builds release binary, installs config + unit
```

Then:

```sh
ls -l /dev/disk/by-id/        # find your flash device
$EDITOR /etc/umami/umami.toml # set tiers.umami_device
sudo umami setup --format     # mkswap + swapon all tiers (once)
sudo systemctl enable --now umami.service
journalctl -u umami -f
```

Optional: install `systemd/99-umami.rules` to `/etc/udev/rules.d/` to
auto-start the daemon when a swap-formatted device is plugged in.

## CLI

```
umami daemon      run the control loop (what systemd starts)
umami setup       configure tiers idempotently; --format also runs mkswap
umami status      memory, PSI, active swap tiers, current vm tunables
umami teardown    swapoff tiers, remove ZRAM, restore stock sysctls
```

Config search order: `--config PATH`, `/etc/umami/umami.toml`,
`./umami.toml`, built-in defaults. See `config/umami.toml` for every knob.

## Failure modes, honestly

- **USB disconnect with active swap** can stall processes faulting on
  those pages. Umami's watchdog stops *new* writes immediately, but it
  deliberately does **not** auto-`swapoff` a dead device (that can hang
  in D-state). Use quality ports/hubs; prefer by-id device paths.
- **Flash wear**: real, mitigated by ZRAM-first ordering and lz4.
  Don't point this at a bargain QLC stick and expect it to live forever.
- **Not a fix for bad software**: Umami prevents crashes and freezes,
  not runaway leaks. If something allocates unboundedly, you now lose
  gracefully instead of hard.

## Application-level spillover

For memory-hostile workloads (builds, browsers, model tooling), point
their scratch space at a fast mount to bypass kernel guesswork:

```sh
export UMAMI_TMPDIR=/mnt/fast/tmp
export TMPDIR="$UMAMI_TMPDIR"
```

## Development

Zero external dependencies; Rust 2024 edition, stdlib only.

```sh
cargo build
cargo test    # parsers, swap-rate math, full policy state machine
```

Layout:

- `src/procfs.rs` — kernel interface sampling (meminfo, vmstat, PSI)
- `src/policy.rs` — pure, unit-tested control policy / state machine
- `src/tiers.rs` — actuators: sysctl, swapon/swapoff, ZRAM lifecycle
- `src/config.rs` — minimal TOML-subset config parser
- `src/main.rs` — daemon loop, watchdog, CLI
