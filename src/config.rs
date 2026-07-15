//! Configuration: a minimal TOML subset (sections, `key = value` scalars,
//! `#` comments) so the daemon stays dependency-free.

use std::collections::HashMap;
use std::fs;
use std::io;

#[derive(Debug, Clone, PartialEq)]
enum Value {
    Int(i64),
    Float(f64),
    Str(String),
}

#[derive(Debug, Default)]
pub struct Config {
    values: HashMap<(String, String), Value>,
}

impl Config {
    pub fn from_str(text: &str) -> Result<Config, String> {
        let mut cfg = Config::default();
        let mut section = String::new();
        for (lineno, raw) in text.lines().enumerate() {
            let line = strip_comment(raw).trim();
            if line.is_empty() {
                continue;
            }
            if line.starts_with('[') {
                if !line.ends_with(']') || line.len() <= 2 {
                    return Err(format!("line {}: malformed section header", lineno + 1));
                }
                section = line[1..line.len() - 1].trim().to_string();
                if section.is_empty() {
                    return Err(format!("line {}: empty section name", lineno + 1));
                }
                continue;
            }
            let Some((key, val)) = line.split_once('=') else {
                return Err(format!("line {}: expected `key = value`", lineno + 1));
            };
            let key = key.trim();
            if key.is_empty() {
                return Err(format!("line {}: empty key", lineno + 1));
            }
            if section.is_empty() {
                return Err(format!("line {}: key outside any section", lineno + 1));
            }
            let value = parse_value(val.trim()).map_err(|e| format!("line {}: {}", lineno + 1, e))?;
            cfg.values.insert((section.clone(), key.to_string()), value);
        }
        Ok(cfg)
    }

    pub fn load(path: &str) -> io::Result<Config> {
        let text = fs::read_to_string(path)?;
        Config::from_str(&text)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{}: {}", path, e)))
    }

    fn get(&self, section: &str, key: &str) -> Option<&Value> {
        self.values.get(&(section.to_string(), key.to_string()))
    }

    fn int_or(&self, section: &str, key: &str, default: i64) -> i64 {
        match self.get(section, key) {
            Some(Value::Int(v)) => *v,
            Some(Value::Float(v)) => *v as i64,
            _ => default,
        }
    }

    fn float_or(&self, section: &str, key: &str, default: f64) -> f64 {
        match self.get(section, key) {
            Some(Value::Float(v)) => *v,
            Some(Value::Int(v)) => *v as f64,
            _ => default,
        }
    }

    fn str_or(&self, section: &str, key: &str, default: &str) -> String {
        match self.get(section, key) {
            Some(Value::Str(v)) => v.clone(),
            _ => default.to_string(),
        }
    }

    pub fn daemon(&self) -> DaemonCfg {
        DaemonCfg {
            interval_ms: self.int_or("daemon", "interval_ms", 1000).max(100) as u64,
        }
    }

    pub fn tiers(&self) -> TiersCfg {
        TiersCfg {
            zram_size_mb: self.int_or("tiers", "zram_size_mb", 8192).max(0) as u64,
            zram_algorithm: self.str_or("tiers", "zram_algorithm", "lz4"),
            zram_priority: self.int_or("tiers", "zram_priority", 200) as i32,
            umami_device: self.str_or("tiers", "umami_device", ""),
            umami_priority: self.int_or("tiers", "umami_priority", 100) as i32,
            fallback_swap: self.str_or("tiers", "fallback_swap", ""),
            fallback_priority: self.int_or("tiers", "fallback_priority", 10) as i32,
        }
    }

    pub fn policy(&self) -> PolicyCfg {
        PolicyCfg {
            swappiness_calm: self.int_or("policy", "swappiness_calm", 60).clamp(0, 200) as u32,
            swappiness_pressured: self.int_or("policy", "swappiness_pressured", 120).clamp(0, 200) as u32,
            swappiness_critical: self.int_or("policy", "swappiness_critical", 180).clamp(0, 200) as u32,
            swappiness_thrash_guard: self.int_or("policy", "swappiness_thrash_guard", 30).clamp(0, 200) as u32,
            cache_pressure_calm: self.int_or("policy", "cache_pressure_calm", 100).max(1) as u32,
            cache_pressure_pressured: self.int_or("policy", "cache_pressure_pressured", 50).max(1) as u32,
            psi_pressure_threshold: self.float_or("policy", "psi_pressure_threshold", 5.0),
            psi_critical_threshold: self.float_or("policy", "psi_critical_threshold", 20.0),
            mem_pressure_available_pct: self.float_or("policy", "mem_pressure_available_pct", 15.0),
            mem_critical_available_pct: self.float_or("policy", "mem_critical_available_pct", 5.0),
            thrash_swapin_kbs: self.float_or("policy", "thrash_swapin_kbs", 51200.0),
            thrash_psi_full_avg10: self.float_or("policy", "thrash_psi_full_avg10", 5.0),
            enter_samples: self.int_or("policy", "enter_samples", 2).max(1) as u32,
            exit_samples: self.int_or("policy", "exit_samples", 10).max(1) as u32,
            watchdog_swappiness: self.int_or("policy", "watchdog_swappiness", 10).clamp(0, 200) as u32,
        }
    }
}

fn strip_comment(line: &str) -> &str {
    let mut in_string = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => in_string = !in_string,
            '#' if !in_string => return &line[..i],
            _ => {}
        }
    }
    line
}

fn parse_value(s: &str) -> Result<Value, String> {
    if s.len() >= 2 && s.starts_with('"') && s.ends_with('"') {
        return Ok(Value::Str(s[1..s.len() - 1].to_string()));
    }
    if let Ok(v) = s.parse::<i64>() {
        return Ok(Value::Int(v));
    }
    if let Ok(v) = s.parse::<f64>() {
        return Ok(Value::Float(v));
    }
    Err(format!("invalid value: {:?}", s))
}

#[derive(Debug, Clone)]
pub struct DaemonCfg {
    pub interval_ms: u64,
}

#[derive(Debug, Clone)]
pub struct TiersCfg {
    /// ZRAM swap size in MiB; 0 disables the ZRAM tier.
    pub zram_size_mb: u64,
    pub zram_algorithm: String,
    pub zram_priority: i32,
    /// Block device for the flash tier; empty means unmanaged.
    pub umami_device: String,
    pub umami_priority: i32,
    /// Last-resort disk swap path; empty means unmanaged.
    pub fallback_swap: String,
    pub fallback_priority: i32,
}

#[derive(Debug, Clone)]
pub struct PolicyCfg {
    pub swappiness_calm: u32,
    pub swappiness_pressured: u32,
    pub swappiness_critical: u32,
    pub swappiness_thrash_guard: u32,
    pub cache_pressure_calm: u32,
    pub cache_pressure_pressured: u32,
    pub psi_pressure_threshold: f64,
    pub psi_critical_threshold: f64,
    pub mem_pressure_available_pct: f64,
    pub mem_critical_available_pct: f64,
    pub thrash_swapin_kbs: f64,
    pub thrash_psi_full_avg10: f64,
    pub enter_samples: u32,
    pub exit_samples: u32,
    /// Swappiness pinned while the flash tier is missing (USB pulled).
    pub watchdog_swappiness: u32,
}

impl PolicyCfg {
    pub fn validate(&self) -> Result<(), String> {
        if self.psi_pressure_threshold >= self.psi_critical_threshold {
            return Err("psi_pressure_threshold must be < psi_critical_threshold".into());
        }
        if self.mem_pressure_available_pct <= self.mem_critical_available_pct {
            return Err("mem_pressure_available_pct must be > mem_critical_available_pct".into());
        }
        if !(0.0..=100.0).contains(&self.mem_pressure_available_pct) {
            return Err("mem_pressure_available_pct out of range".into());
        }
        if self.thrash_swapin_kbs <= 0.0 {
            return Err("thrash_swapin_kbs must be positive".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = r#"
# Umami example configuration
[daemon]
interval_ms = 1000

[tiers]
zram_size_mb = 8192
zram_algorithm = "lz4"
zram_priority = 200
umami_device = "/dev/disk/by-id/usb-flash-part1"  # comment after value
umami_priority = 100
fallback_swap = "/swapfile"
fallback_priority = 10

[policy]
swappiness_calm = 60
swappiness_pressured = 120
swappiness_critical = 180
swappiness_thrash_guard = 30
cache_pressure_calm = 100
cache_pressure_pressured = 50
psi_pressure_threshold = 5.0
psi_critical_threshold = 20.0
mem_pressure_available_pct = 15
mem_critical_available_pct = 5
thrash_swapin_kbs = 51200
thrash_psi_full_avg10 = 5.0
enter_samples = 2
exit_samples = 10
"#;

    #[test]
    fn parses_example() {
        let cfg = Config::from_str(EXAMPLE).unwrap();
        let d = cfg.daemon();
        assert_eq!(d.interval_ms, 1000);
        let t = cfg.tiers();
        assert_eq!(t.zram_size_mb, 8192);
        assert_eq!(t.zram_algorithm, "lz4");
        assert_eq!(t.zram_priority, 200);
        assert_eq!(t.umami_device, "/dev/disk/by-id/usb-flash-part1");
        assert_eq!(t.fallback_priority, 10);
        let p = cfg.policy();
        assert_eq!(p.swappiness_critical, 180);
        assert_eq!(p.mem_pressure_available_pct, 15.0); // int coerced to float
        assert_eq!(p.thrash_swapin_kbs, 51200.0);
        p.validate().unwrap();
    }

    #[test]
    fn defaults_when_empty() {
        let cfg = Config::from_str("").unwrap();
        let p = cfg.policy();
        assert_eq!(p.swappiness_calm, 60);
        assert_eq!(p.exit_samples, 10);
        let t = cfg.tiers();
        assert_eq!(t.umami_device, "");
        p.validate().unwrap();
    }

    #[test]
    fn hash_inside_string_is_not_a_comment() {
        let cfg = Config::from_str("[a]\nb = \"x#y\"\n").unwrap();
        assert_eq!(cfg.str_or("a", "b", ""), "x#y");
    }

    #[test]
    fn rejects_malformed() {
        assert!(Config::from_str("key = 1\n").is_err()); // outside section
        assert!(Config::from_str("[a]\njust words\n").is_err());
        assert!(Config::from_str("[a]\nk = @@@\n").is_err());
        assert!(Config::from_str("[a\nk = 1\n").is_err());
    }

    #[test]
    fn shipped_example_config_is_valid() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/config/umami.toml");
        let cfg = Config::load(path).unwrap();
        cfg.policy().validate().unwrap();
    }

    #[test]
    fn validation_catches_inverted_thresholds() {
        let cfg = Config::from_str("[policy]\npsi_pressure_threshold = 30\npsi_critical_threshold = 10\n").unwrap();
        assert!(cfg.policy().validate().is_err());
    }
}
