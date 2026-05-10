use anyhow::{Context, Result};
use std::time::Duration;
use tokio::time::sleep;

#[derive(Debug)]
pub struct CpuSample {
    pub usage_pct: f64,
}

#[derive(Debug)]
pub struct MemSample {
    pub used_mb:   f64,
    pub free_mb:   f64,
    pub used_pct:  f64,
}

#[derive(Debug)]
pub struct LoadSample {
    pub avg_1m: f64,
}

/// Reads /proc/stat twice 100 ms apart and returns CPU usage %.
pub async fn collect_cpu() -> Result<CpuSample> {
    let (user1, nice1, system1, idle1, iowait1) = read_cpu_stat()?;
    sleep(Duration::from_millis(100)).await;
    let (user2, nice2, system2, idle2, iowait2) = read_cpu_stat()?;

    let total1 = user1 + nice1 + system1 + idle1 + iowait1;
    let total2 = user2 + nice2 + system2 + idle2 + iowait2;
    let idle_diff = (idle2 + iowait2) - (idle1 + iowait1);
    let total_diff = total2 - total1;

    let usage_pct = if total_diff == 0 {
        0.0
    } else {
        100.0 * (total_diff - idle_diff) as f64 / total_diff as f64
    };

    Ok(CpuSample { usage_pct })
}

fn read_cpu_stat() -> Result<(u64, u64, u64, u64, u64)> {
    let content = std::fs::read_to_string("/proc/stat").context("read /proc/stat")?;
    let line = content
        .lines()
        .find(|l| l.starts_with("cpu "))
        .context("no cpu line in /proc/stat")?;

    let nums: Vec<u64> = line
        .split_whitespace()
        .skip(1)
        .take(5)
        .map(|s| s.parse().unwrap_or(0))
        .collect();

    Ok((
        nums.first().copied().unwrap_or(0),
        nums.get(1).copied().unwrap_or(0),
        nums.get(2).copied().unwrap_or(0),
        nums.get(3).copied().unwrap_or(0),
        nums.get(4).copied().unwrap_or(0),
    ))
}

pub fn collect_memory() -> Result<MemSample> {
    let content = std::fs::read_to_string("/proc/meminfo").context("read /proc/meminfo")?;
    let mut total_kb: u64 = 0;
    let mut available_kb: u64 = 0;

    for line in content.lines() {
        if let Some(val) = parse_meminfo_line(line, "MemTotal:") {
            total_kb = val;
        } else if let Some(val) = parse_meminfo_line(line, "MemAvailable:") {
            available_kb = val;
        }
    }

    let used_kb = total_kb.saturating_sub(available_kb);
    let used_mb = used_kb as f64 / 1024.0;
    let free_mb = available_kb as f64 / 1024.0;
    let used_pct = if total_kb == 0 {
        0.0
    } else {
        100.0 * used_kb as f64 / total_kb as f64
    };

    Ok(MemSample { used_mb, free_mb, used_pct })
}

fn parse_meminfo_line(line: &str, key: &str) -> Option<u64> {
    line.strip_prefix(key)?
        .split_whitespace()
        .next()?
        .parse()
        .ok()
}

pub fn collect_load() -> Result<LoadSample> {
    let content = std::fs::read_to_string("/proc/loadavg").context("read /proc/loadavg")?;
    let avg_1m = content
        .split_whitespace()
        .next()
        .context("empty /proc/loadavg")?
        .parse::<f64>()
        .context("parse load avg")?;
    Ok(LoadSample { avg_1m })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_collector_returns_plausible_values() {
        let m = collect_memory().expect("collect_memory failed");
        assert!(m.used_mb >= 0.0, "used_mb must be non-negative");
        assert!(m.free_mb >= 0.0, "free_mb must be non-negative");
        assert!(m.used_pct >= 0.0 && m.used_pct <= 100.0, "used_pct must be 0-100");
    }

    #[test]
    fn load_collector_returns_non_negative() {
        let l = collect_load().expect("collect_load failed");
        assert!(l.avg_1m >= 0.0);
    }

    #[tokio::test]
    async fn cpu_collector_returns_value_in_range() {
        let c = collect_cpu().await.expect("collect_cpu failed");
        assert!(c.usage_pct >= 0.0 && c.usage_pct <= 100.0,
            "cpu_usage_pct out of range: {}", c.usage_pct);
    }
}
