//! Sampling of the kernel interfaces Umami reads:
//! /proc/meminfo, /proc/vmstat and /proc/pressure/memory.

use std::fs;
use std::io;
use std::time::Instant;

const MEMINFO: &str = "/proc/meminfo";
const VMSTAT: &str = "/proc/vmstat";
const PSI_MEMORY: &str = "/proc/pressure/memory";
const AUXV: &str = "/proc/self/auxv";

/// One instantaneous view of system memory pressure.
#[derive(Debug, Clone, Copy)]
pub struct Sample {
    /// MemAvailable as a percentage of MemTotal.
    pub mem_available_pct: f64,
    /// Used swap as a percentage of total swap (0 when no swap exists).
    pub swap_used_pct: f64,
    /// PSI "some" avg10 for memory: % of wall time any task stalled on memory.
    pub psi_some_avg10: f64,
    /// PSI "full" avg10 for memory: % of wall time all non-idle tasks stalled.
    pub psi_full_avg10: f64,
    /// Swap-in rate over the sampling interval, KiB/s.
    pub swapin_kbs: f64,
    /// Swap-out rate over the sampling interval, KiB/s.
    pub swapout_kbs: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemInfo {
    pub total_kb: u64,
    pub available_kb: u64,
    pub swap_total_kb: u64,
    pub swap_free_kb: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VmStat {
    /// Pages swapped in since boot.
    pub pswpin: u64,
    /// Pages swapped out since boot.
    pub pswpout: u64,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Psi {
    pub some_avg10: f64,
    pub some_avg60: f64,
    pub some_avg300: f64,
    pub full_avg10: f64,
    pub full_avg60: f64,
    pub full_avg300: f64,
}

pub fn read_meminfo() -> io::Result<MemInfo> {
    parse_meminfo(&fs::read_to_string(MEMINFO)?)
}

pub fn read_vmstat() -> io::Result<VmStat> {
    parse_vmstat(&fs::read_to_string(VMSTAT)?)
}

pub fn read_memory_psi() -> io::Result<Psi> {
    parse_psi(&fs::read_to_string(PSI_MEMORY)?)
}

fn parse_meminfo(text: &str) -> io::Result<MemInfo> {
    let mut m = MemInfo {
        total_kb: 0,
        available_kb: 0,
        swap_total_kb: 0,
        swap_free_kb: 0,
    };
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let Some(key) = it.next() else { continue };
        let Some(val) = it.next().and_then(|v| v.parse::<u64>().ok()) else {
            continue;
        };
        match key {
            "MemTotal:" => m.total_kb = val,
            "MemAvailable:" => m.available_kb = val,
            "SwapTotal:" => m.swap_total_kb = val,
            "SwapFree:" => m.swap_free_kb = val,
            _ => {}
        }
    }
    if m.total_kb == 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "MemTotal missing from /proc/meminfo",
        ));
    }
    Ok(m)
}

fn parse_vmstat(text: &str) -> io::Result<VmStat> {
    let mut vm = VmStat::default();
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let (Some(key), Some(val)) = (it.next(), it.next()) else {
            continue;
        };
        let Ok(val) = val.parse::<u64>() else { continue };
        match key {
            "pswpin" => vm.pswpin = val,
            "pswpout" => vm.pswpout = val,
            _ => {}
        }
    }
    Ok(vm)
}

fn parse_psi(text: &str) -> io::Result<Psi> {
    let mut psi = Psi {
        some_avg10: 0.0,
        some_avg60: 0.0,
        some_avg300: 0.0,
        full_avg10: 0.0,
        full_avg60: 0.0,
        full_avg300: 0.0,
    };
    for line in text.lines() {
        let mut it = line.split_whitespace();
        let Some(scope) = it.next() else { continue };
        for kv in it {
            let Some((k, v)) = kv.split_once('=') else { continue };
            let Ok(v) = v.parse::<f64>() else { continue };
            match (scope, k) {
                ("some", "avg10") => psi.some_avg10 = v,
                ("some", "avg60") => psi.some_avg60 = v,
                ("some", "avg300") => psi.some_avg300 = v,
                ("full", "avg10") => psi.full_avg10 = v,
                ("full", "avg60") => psi.full_avg60 = v,
                ("full", "avg300") => psi.full_avg300 = v,
                _ => {}
            }
        }
    }
    Ok(psi)
}

/// Swap-in / swap-out rates in KiB/s between two vmstat snapshots.
/// Saturating: a counter reset (or boot wrap) yields 0, not a huge number.
pub fn swap_rates(prev: &VmStat, cur: &VmStat, elapsed_secs: f64, page_kb: u64) -> (f64, f64) {
    if elapsed_secs <= 0.0 || page_kb == 0 {
        return (0.0, 0.0);
    }
    let pages_in = cur.pswpin.saturating_sub(prev.pswpin) as f64;
    let pages_out = cur.pswpout.saturating_sub(prev.pswpout) as f64;
    (
        pages_in * page_kb as f64 / elapsed_secs,
        pages_out * page_kb as f64 / elapsed_secs,
    )
}

/// Kernel page size in KiB, read from AT_PAGESZ in the aux vector.
fn page_size_kb() -> u64 {
    const AT_PAGESZ: u64 = 6;
    if let Ok(bytes) = fs::read(AUXV) {
        // 64-bit auxv: pairs of native-endian u64. Anything else falls back to 4 KiB.
        for pair in bytes.chunks_exact(16) {
            let key = u64::from_ne_bytes(pair[0..8].try_into().unwrap());
            let val = u64::from_ne_bytes(pair[8..16].try_into().unwrap());
            if key == AT_PAGESZ && val >= 1024 {
                return val / 1024;
            }
        }
    }
    4
}

/// Samples the kernel interfaces and derives rates between consecutive samples.
pub struct Monitor {
    page_kb: u64,
    last: Option<(Instant, VmStat)>,
}

impl Default for Monitor {
    fn default() -> Self {
        Self::new()
    }
}

impl Monitor {
    pub fn new() -> Monitor {
        Monitor {
            page_kb: page_size_kb(),
            last: None,
        }
    }

    pub fn sample(&mut self) -> io::Result<Sample> {
        let mem = read_meminfo()?;
        let psi = read_memory_psi()?;
        let vm = read_vmstat()?;
        let now = Instant::now();
        let (swapin_kbs, swapout_kbs) = match &self.last {
            Some((t0, prev)) => swap_rates(prev, &vm, now.duration_since(*t0).as_secs_f64(), self.page_kb),
            None => (0.0, 0.0),
        };
        self.last = Some((now, vm));
        Ok(Sample {
            mem_available_pct: mem.available_kb as f64 * 100.0 / mem.total_kb as f64,
            swap_used_pct: if mem.swap_total_kb == 0 {
                0.0
            } else {
                (mem.swap_total_kb - mem.swap_free_kb) as f64 * 100.0 / mem.swap_total_kb as f64
            },
            psi_some_avg10: psi.some_avg10,
            psi_full_avg10: psi.full_avg10,
            swapin_kbs,
            swapout_kbs,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_meminfo() {
        let text = "\
MemTotal:        7724580 kB
MemFree:          123456 kB
MemAvailable:    1998644 kB
Buffers:          100000 kB
SwapTotal:      11918836 kB
SwapFree:        8455536 kB
";
        let m = parse_meminfo(text).unwrap();
        assert_eq!(m.total_kb, 7724580);
        assert_eq!(m.available_kb, 1998644);
        assert_eq!(m.swap_total_kb, 11918836);
        assert_eq!(m.swap_free_kb, 8455536);
    }

    #[test]
    fn rejects_meminfo_without_total() {
        assert!(parse_meminfo("MemFree: 1 kB\n").is_err());
    }

    #[test]
    fn parses_vmstat() {
        let text = "nr_free_pages 100\npswpin 123456\npswpout 654321\nnr_swapcache 5\n";
        let vm = parse_vmstat(text).unwrap();
        assert_eq!(vm.pswpin, 123456);
        assert_eq!(vm.pswpout, 654321);
    }

    #[test]
    fn parses_psi() {
        let text = "some avg10=1.50 avg60=0.75 avg300=0.25 total=1382494496\n\
                    full avg10=0.50 avg60=0.05 avg300=0.01 total=1265083042\n";
        let psi = parse_psi(text).unwrap();
        assert_eq!(psi.some_avg10, 1.50);
        assert_eq!(psi.some_avg60, 0.75);
        assert_eq!(psi.some_avg300, 0.25);
        assert_eq!(psi.full_avg10, 0.50);
        assert_eq!(psi.full_avg60, 0.05);
        assert_eq!(psi.full_avg300, 0.01);
    }

    #[test]
    fn computes_swap_rates() {
        let prev = VmStat { pswpin: 1000, pswpout: 2000 };
        let cur = VmStat { pswpin: 1500, pswpout: 4000 };
        let (rin, rout) = swap_rates(&prev, &cur, 2.0, 4);
        assert_eq!(rin, 1000.0); // 500 pages * 4 KiB / 2 s
        assert_eq!(rout, 4000.0);
    }

    #[test]
    fn swap_rates_handle_zero_elapsed_and_counter_reset() {
        let prev = VmStat { pswpin: 1000, pswpout: 2000 };
        let cur = VmStat { pswpin: 10, pswpout: 20 }; // counters went backwards
        assert_eq!(swap_rates(&prev, &cur, 0.0, 4), (0.0, 0.0));
        assert_eq!(swap_rates(&prev, &cur, 1.0, 4), (0.0, 0.0));
    }

    #[test]
    fn live_monitor_smoke() {
        let mut mon = Monitor::new();
        assert!(mon.page_kb >= 4);
        let s1 = mon.sample().unwrap();
        std::thread::sleep(std::time::Duration::from_millis(20));
        let s2 = mon.sample().unwrap();
        assert!(s1.mem_available_pct > 0.0 && s1.mem_available_pct <= 100.0);
        assert_eq!(s1.swapin_kbs, 0.0); // no previous sample
        assert!(s2.swapin_kbs >= 0.0 && s2.swapout_kbs >= 0.0);
        assert!(s2.psi_some_avg10 >= 0.0);
    }
}
