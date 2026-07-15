//! Actuators: sysctl writes, swap device management, ZRAM setup.

use std::fs;
use std::io;
use std::path::Path;
use std::process::Command;

pub const SWAPPINESS: &str = "/proc/sys/vm/swappiness";
pub const CACHE_PRESSURE: &str = "/proc/sys/vm/vfs_cache_pressure";

pub fn sysctl_write(path: &str, value: u32) -> io::Result<()> {
    fs::write(path, value.to_string().as_bytes())
}

pub fn sysctl_read(path: &str) -> io::Result<u32> {
    fs::read_to_string(path)?
        .trim()
        .parse::<u32>()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))
}

pub fn device_present(dev: &str) -> bool {
    Path::new(dev).exists()
}

/// Device/file paths of currently active swap areas (first column of /proc/swaps).
pub fn active_swaps() -> io::Result<Vec<String>> {
    let text = fs::read_to_string("/proc/swaps")?;
    Ok(text
        .lines()
        .skip(1)
        .filter_map(|l| l.split_whitespace().next())
        .map(str::to_string)
        .collect())
}

fn run(cmd: &str, args: &[&str]) -> io::Result<()> {
    let out = Command::new(cmd).args(args).output()?;
    if out.status.success() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} {} exited with {}: {}",
            cmd,
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        )))
    }
}

pub fn mkswap(dev: &str) -> io::Result<()> {
    run("mkswap", &["-f", dev])
}

pub fn swapon(dev: &str, priority: i32) -> io::Result<()> {
    run("swapon", &["--priority", &priority.to_string(), dev])
}

pub fn swapoff(dev: &str) -> io::Result<()> {
    run("swapoff", &[dev])
}

/// Create a ZRAM device, format it as swap and enable it.
/// Returns the device path, e.g. "/dev/zram0".
pub fn zram_setup(size_mb: u64, algorithm: &str, priority: i32) -> io::Result<String> {
    if !Path::new("/sys/class/zram-control").exists() {
        // Best effort: fails harmlessly if the module is built-in or already loaded.
        let _ = run("modprobe", &["zram"]);
    }
    let hot_add = Path::new("/sys/class/zram-control/hot_add");
    if !hot_add.exists() {
        return Err(io::Error::other(
            "zram-control not available (kernel lacks zram support)",
        ));
    }
    // Reading hot_add allocates a new device and returns its id.
    let id = fs::read_to_string(hot_add)?.trim().to_string();
    let dev = format!("/dev/zram{}", id);
    let sysfs = format!("/sys/block/zram{}", id);

    let result = (|| -> io::Result<()> {
        if !algorithm.is_empty() {
            // Must be set before disksize.
            fs::write(format!("{}/comp_algorithm", sysfs), algorithm)?;
        }
        fs::write(format!("{}/disksize", sysfs), format!("{}M", size_mb))?;
        mkswap(&dev)?;
        swapon(&dev, priority)?;
        Ok(())
    })();

    if let Err(e) = result {
        // Roll back the half-configured device.
        let _ = fs::write("/sys/class/zram-control/hot_remove", &id);
        return Err(e);
    }
    Ok(dev)
}

/// Disable a ZRAM swap device and remove it.
pub fn zram_teardown(dev: &str) -> io::Result<()> {
    let Some(id) = dev.strip_prefix("/dev/zram") else {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("not a zram device: {}", dev),
        ));
    };
    let _ = swapoff(dev);
    let sysfs = format!("/sys/block/zram{}", id);
    if Path::new(&sysfs).exists() {
        fs::write(format!("{}/reset", sysfs), "1")?;
        fs::write("/sys/class/zram-control/hot_remove", id)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_sysctls() {
        // Any Linux host exposes both of these.
        assert!(sysctl_read(SWAPPINESS).is_ok());
        assert!(sysctl_read(CACHE_PRESSURE).is_ok());
    }

    #[test]
    fn lists_active_swaps() {
        // Must parse without error; the list itself may be empty.
        assert!(active_swaps().is_ok());
    }
}
