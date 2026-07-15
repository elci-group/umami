//! umami — multi-tier memory pressure buffering daemon.
//!
//! Tier order: RAM -> ZRAM (compressed, pri 200) -> Umami flash (pri 100)
//! -> disk swap (pri 10). The daemon watches PSI / MemAvailable / swap IO
//! and reshapes vm.swappiness + vm.vfs_cache_pressure through a hysteretic
//! control policy (see policy.rs).

mod config;
mod policy;
mod procfs;
mod tiers;

use std::io;
use std::path::Path;
use std::process::ExitCode;
use std::thread;
use std::time::{Duration, Instant};

use config::{Config, DaemonCfg, PolicyCfg, TiersCfg};
use policy::Policy;
use procfs::Monitor;

const DEFAULT_CONFIG: &str = "/etc/umami/umami.toml";
const LOCAL_CONFIG: &str = "./umami.toml";

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let Some(cmd) = args.first() else {
        usage();
        return ExitCode::from(2);
    };
    let config_flag = flag_value(&args, "--config");
    match cmd.as_str() {
        "daemon" => run_daemon(config_flag),
        "setup" => run_setup(config_flag, args.iter().any(|a| a == "--format")),
        "teardown" => run_teardown(config_flag),
        "status" => run_status(),
        "help" | "--help" | "-h" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("unknown command: {}", other);
            usage();
            ExitCode::from(2)
        }
    }
}

fn usage() {
    eprintln!(
        "umami — memory pressure buffering daemon\n\
         \n\
         usage: umami <command> [--config PATH] [--format]\n\
         \n\
         commands:\n\
         \x20 daemon      run the control loop in the foreground\n\
         \x20 setup       configure tiers (ZRAM, flash, fallback); --format also runs mkswap\n\
         \x20 teardown    swapoff managed tiers, remove ZRAM, restore sysctl defaults\n\
         \x20 status      print memory pressure, swap tiers and current vm tunables\n\
         \x20 help        this text\n\
         \n\
         config search order: --config, {DEFAULT}, {LOCAL}, built-in defaults",
        DEFAULT = DEFAULT_CONFIG,
        LOCAL = LOCAL_CONFIG,
    );
}

fn flag_value(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// Timestamped logging to stderr; journald adds its own stamps on top.
struct Log {
    t0: Instant,
}

impl Log {
    fn new() -> Log {
        Log { t0: Instant::now() }
    }
    fn line(&self, level: &str, msg: &str) {
        eprintln!("[{:>8.1}s] [{:<5}] {}", self.t0.elapsed().as_secs_f64(), level, msg);
    }
    fn info(&self, msg: &str) {
        self.line("INFO", msg);
    }
    fn warn(&self, msg: &str) {
        self.line("WARN", msg);
    }
    fn error(&self, msg: &str) {
        self.line("ERROR", msg);
    }
}

fn load_config(flag: Option<String>, log: &Log) -> io::Result<Config> {
    let candidates: Vec<String> = match flag {
        Some(p) => vec![p],
        None => vec![DEFAULT_CONFIG.to_string(), LOCAL_CONFIG.to_string()],
    };
    for path in &candidates {
        if Path::new(path).exists() {
            let cfg = Config::load(path)?;
            log.info(&format!("config: {}", path));
            return Ok(cfg);
        }
    }
    log.warn("no config file found; using built-in defaults");
    Ok(Config::default())
}

/// Bring up every configured tier. Best effort: each tier logs its own
/// failure and the daemon keeps running in a degraded configuration.
fn ensure_tiers(cfg: &TiersCfg, format: bool, log: &Log) {
    let swaps = tiers::active_swaps().unwrap_or_default();

    // ZRAM tier: compressed RAM, absorbs pressure before flash sees a byte.
    if cfg.zram_size_mb > 0 && !swaps.iter().any(|s| s.starts_with("/dev/zram")) {
        match tiers::zram_setup(cfg.zram_size_mb, &cfg.zram_algorithm, cfg.zram_priority) {
            Ok(dev) => log.info(&format!(
                "zram tier up: {} ({} MiB, {}, priority {})",
                dev, cfg.zram_size_mb, cfg.zram_algorithm, cfg.zram_priority
            )),
            Err(e) => log.warn(&format!("zram tier unavailable: {}", e)),
        }
    }

    // Umami flash tier.
    if !cfg.umami_device.is_empty() && !swaps.contains(&cfg.umami_device) {
        if !tiers::device_present(&cfg.umami_device) {
            log.warn(&format!("umami tier {} not present; skipping", cfg.umami_device));
        } else {
            if format {
                match tiers::mkswap(&cfg.umami_device) {
                    Ok(()) => log.info(&format!("formatted {} as swap", cfg.umami_device)),
                    Err(e) => log.warn(&format!("mkswap {} failed: {}", cfg.umami_device, e)),
                }
            }
            match tiers::swapon(&cfg.umami_device, cfg.umami_priority) {
                Ok(()) => log.info(&format!(
                    "umami tier up: {} (priority {})",
                    cfg.umami_device, cfg.umami_priority
                )),
                Err(e) => log.warn(&format!(
                    "umami tier {} failed to activate: {} (needs `umami setup --format`?)",
                    cfg.umami_device, e
                )),
            }
        }
    }

    // Disk fallback tier: last resort only.
    if !cfg.fallback_swap.is_empty() && !swaps.contains(&cfg.fallback_swap) {
        if !tiers::device_present(&cfg.fallback_swap) {
            log.warn(&format!("fallback swap {} not present; skipping", cfg.fallback_swap));
        } else {
            if format {
                match tiers::mkswap(&cfg.fallback_swap) {
                    Ok(()) => log.info(&format!("formatted {} as swap", cfg.fallback_swap)),
                    Err(e) => log.warn(&format!("mkswap {} failed: {}", cfg.fallback_swap, e)),
                }
            }
            match tiers::swapon(&cfg.fallback_swap, cfg.fallback_priority) {
                Ok(()) => log.info(&format!(
                    "fallback tier up: {} (priority {})",
                    cfg.fallback_swap, cfg.fallback_priority
                )),
                Err(e) => log.warn(&format!("fallback tier {} failed: {}", cfg.fallback_swap, e)),
            }
        }
    }
}

fn run_daemon(config_flag: Option<String>) -> ExitCode {
    let log = Log::new();
    let cfg = match load_config(config_flag, &log) {
        Ok(c) => c,
        Err(e) => {
            log.error(&format!("failed to load config: {}", e));
            return ExitCode::from(2);
        }
    };
    let policy_cfg: PolicyCfg = cfg.policy();
    if let Err(e) = policy_cfg.validate() {
        log.error(&format!("invalid policy config: {}", e));
        return ExitCode::from(2);
    }
    let tiers_cfg: TiersCfg = cfg.tiers();
    let daemon_cfg: DaemonCfg = cfg.daemon();

    log.info(&format!(
        "umami starting: zram={}MiB flash={} fallback={}",
        tiers_cfg.zram_size_mb,
        if tiers_cfg.umami_device.is_empty() { "unmanaged" } else { &tiers_cfg.umami_device },
        if tiers_cfg.fallback_swap.is_empty() { "unmanaged" } else { &tiers_cfg.fallback_swap },
    ));

    ensure_tiers(&tiers_cfg, false, &log);

    let mut monitor = Monitor::new();
    let mut policy = Policy::new(policy_cfg.clone());
    let mut applied: Option<(u32, u32)> = None;
    let mut degraded = false;

    loop {
        match monitor.sample() {
            Ok(sample) => {
                let flash_missing = !tiers_cfg.umami_device.is_empty()
                    && !tiers::device_present(&tiers_cfg.umami_device);

                if flash_missing {
                    // Watchdog: the flash tier vanished (USB pulled). Pages already
                    // swapped there are the kernel's problem; stop sending new ones.
                    if !degraded {
                        log.error(&format!(
                            "umami tier {} disconnected; pinning swappiness to {}",
                            tiers_cfg.umami_device, policy_cfg.watchdog_swappiness
                        ));
                        degraded = true;
                        applied = None;
                    }
                    apply(&policy_cfg.watchdog_swappiness, &policy_cfg.cache_pressure_pressured, &mut applied, &log);
                } else {
                    if degraded {
                        log.info("umami tier is back; re-arming");
                        ensure_tiers(&tiers_cfg, false, &log);
                        degraded = false;
                        applied = None;
                    }
                    let d = policy.evaluate(&sample);
                    if d.changed {
                        log.info(&format!(
                            "state -> {:?}: {} (swappiness={}, cache_pressure={}, avail={:.1}%, swap_used={:.1}%, psi_some={:.1}, psi_full={:.1}, in={:.0}KiB/s, out={:.0}KiB/s)",
                            d.state, d.reason, d.swappiness, d.cache_pressure,
                            sample.mem_available_pct, sample.swap_used_pct,
                            sample.psi_some_avg10, sample.psi_full_avg10,
                            sample.swapin_kbs, sample.swapout_kbs,
                        ));
                    }
                    apply(&d.swappiness, &d.cache_pressure, &mut applied, &log);
                }
            }
            Err(e) => log.warn(&format!("sample failed: {}", e)),
        }
        thread::sleep(Duration::from_millis(daemon_cfg.interval_ms));
    }
}

/// Write the two vm tunables, but only when the target actually changed.
fn apply(swappiness: &u32, cache_pressure: &u32, applied: &mut Option<(u32, u32)>, log: &Log) {
    let target = (*swappiness, *cache_pressure);
    if *applied == Some(target) {
        return;
    }
    if let Err(e) = tiers::sysctl_write(tiers::SWAPPINESS, *swappiness) {
        log.warn(&format!("cannot set swappiness={}: {}", swappiness, e));
    }
    if let Err(e) = tiers::sysctl_write(tiers::CACHE_PRESSURE, *cache_pressure) {
        log.warn(&format!("cannot set vfs_cache_pressure={}: {}", cache_pressure, e));
    }
    *applied = Some(target);
}

fn run_setup(config_flag: Option<String>, format: bool) -> ExitCode {
    let log = Log::new();
    let cfg = match load_config(config_flag, &log) {
        Ok(c) => c,
        Err(e) => {
            log.error(&format!("failed to load config: {}", e));
            return ExitCode::from(2);
        }
    };
    if format {
        log.warn("--format given: mkswap will rewrite swap signatures on managed devices");
    }
    ensure_tiers(&cfg.tiers(), format, &log);
    print_swaps();
    ExitCode::SUCCESS
}

fn run_teardown(config_flag: Option<String>) -> ExitCode {
    let log = Log::new();
    let cfg = match load_config(config_flag, &log) {
        Ok(c) => c,
        Err(e) => {
            log.error(&format!("failed to load config: {}", e));
            return ExitCode::from(2);
        }
    };
    let t = cfg.tiers();
    let swaps = tiers::active_swaps().unwrap_or_default();

    for dev in swaps.iter().filter(|s| s.starts_with("/dev/zram")) {
        match tiers::zram_teardown(dev) {
            Ok(()) => log.info(&format!("zram tier {} removed", dev)),
            Err(e) => log.warn(&format!("zram teardown {} failed: {}", dev, e)),
        }
    }
    for dev in [&t.umami_device, &t.fallback_swap] {
        if !dev.is_empty() && swaps.contains(dev) {
            match tiers::swapoff(dev) {
                Ok(()) => log.info(&format!("swapoff {}", dev)),
                Err(e) => log.warn(&format!("swapoff {} failed: {}", dev, e)),
            }
        }
    }

    // Restore distro defaults so the system behaves stock again.
    let _ = tiers::sysctl_write(tiers::SWAPPINESS, 60);
    let _ = tiers::sysctl_write(tiers::CACHE_PRESSURE, 100);
    log.info("sysctl restored: swappiness=60, vfs_cache_pressure=100");
    ExitCode::SUCCESS
}

fn print_swaps() {
    match std::fs::read_to_string("/proc/swaps") {
        Ok(text) => {
            println!("active swap areas:");
            for line in text.lines().skip(1) {
                let f: Vec<&str> = line.split_whitespace().collect();
                if f.len() >= 5 {
                    println!(
                        "  {:<40} {:<10} size={:>8} MiB used={:>8} MiB priority={}",
                        f[0],
                        f[1],
                        f[2].parse::<u64>().unwrap_or(0) / 1024,
                        f[3].parse::<u64>().unwrap_or(0) / 1024,
                        f[4]
                    );
                }
            }
        }
        Err(e) => eprintln!("cannot read /proc/swaps: {}", e),
    }
}

fn run_status() -> ExitCode {
    match procfs::read_meminfo() {
        Ok(m) => {
            let used_swap = m.swap_total_kb.saturating_sub(m.swap_free_kb);
            println!("memory:");
            println!("  total         {:>10} MiB", m.total_kb / 1024);
            println!(
                "  available     {:>10} MiB ({:.1}%)",
                m.available_kb / 1024,
                m.available_kb as f64 * 100.0 / m.total_kb as f64
            );
            println!(
                "  swap          {:>10} MiB total, {} MiB used",
                m.swap_total_kb / 1024,
                used_swap / 1024
            );
        }
        Err(e) => eprintln!("cannot read meminfo: {}", e),
    }
    match procfs::read_memory_psi() {
        Ok(p) => {
            println!("memory pressure (PSI):");
            println!(
                "  some  avg10={:.2} avg60={:.2} avg300={:.2}",
                p.some_avg10, p.some_avg60, p.some_avg300
            );
            println!(
                "  full  avg10={:.2} avg60={:.2} avg300={:.2}",
                p.full_avg10, p.full_avg60, p.full_avg300
            );
        }
        Err(e) => eprintln!("cannot read PSI: {}", e),
    }
    print_swaps();
    println!("vm tunables:");
    match tiers::sysctl_read(tiers::SWAPPINESS) {
        Ok(v) => println!("  vm.swappiness        = {}", v),
        Err(e) => eprintln!("  vm.swappiness: {}", e),
    }
    match tiers::sysctl_read(tiers::CACHE_PRESSURE) {
        Ok(v) => println!("  vm.vfs_cache_pressure = {}", v),
        Err(e) => eprintln!("  vm.vfs_cache_pressure: {}", e),
    }
    ExitCode::SUCCESS
}
