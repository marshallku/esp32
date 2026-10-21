//! The PC's vitals for the panel's stats screen, sampled once a second.
//!
//! Everything comes from files the kernel already exposes, plus `nvidia-smi`
//! for the GPU. Each reading is independent and optional: a missing sensor, a
//! failed read or a first sample with nothing to diff against yields `None`,
//! which the panel draws as a dash — never a made-up zero.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use desk_core::protocol::{Gpu, MAX_CORES, Stats};
use tokio::sync::mpsc;

use crate::session::Event;

const EVERY: Duration = Duration::from_secs(1);
/// nvidia-smi costs tens of milliseconds of CPU, so the GPU is read every
/// other sample and reused in between.
const GPU_EVERY: Duration = Duration::from_secs(2);
const GPU_TIMEOUT: Duration = Duration::from_millis(1500);

pub async fn run(events: mpsc::Sender<Event>) {
    let temp_path = cpu_temp_path();
    let mut prev_cpu: Option<Vec<(u64, u64)>> = None;
    let mut prev_net: Option<(Instant, u64, u64)> = None;
    let mut gpu: (Option<Instant>, Option<Gpu>) = (None, None);
    let mut tick = tokio::time::interval(EVERY);

    loop {
        tick.tick().await;
        let mut stats = Stats::default();

        if let Some(cur) = read("/proc/stat").as_deref().map(parse_proc_stat) {
            if let Some(prev) = &prev_cpu
                && prev.len() == cur.len()
            {
                let busy: Vec<u8> = prev.iter().zip(&cur).map(|(p, c)| busy_percent(*p, *c)).collect();
                stats.cpu = busy.first().copied();
                stats.cores = fold_cores(&busy[1..]);
            }
            prev_cpu = Some(cur);
        }

        if let Some((total, available)) = read("/proc/meminfo").as_deref().and_then(parse_meminfo) {
            stats.mem_total = Some((total / 1024) as u32);
            stats.mem_used = Some((total.saturating_sub(available) / 1024) as u32);
        }

        stats.cpu_temp = temp_path
            .as_ref()
            .and_then(|p| std::fs::read_to_string(p).ok())
            .and_then(|s| s.trim().parse::<i32>().ok())
            .map(|milli| (milli / 1000) as i16);

        if let Some((rx, tx)) = read("/proc/net/dev").as_deref().map(parse_net_dev) {
            let now = Instant::now();
            if let Some((then, prx, ptx)) = prev_net {
                let secs = (now - then).as_secs_f64().max(0.001);
                // A counter that went backwards (interface reset, a device
                // that came and went) gives no rate this sample.
                let rate = |cur: u64, prev: u64| cur.checked_sub(prev).map(|d| (d as f64 / 1024.0 / secs) as u32);
                stats.net_rx = rate(rx, prx);
                stats.net_tx = rate(tx, ptx);
            }
            prev_net = Some((now, rx, tx));
        }

        if gpu.0.is_none_or(|t| t.elapsed() >= GPU_EVERY) {
            gpu = (Some(Instant::now()), read_gpu().await);
        }
        stats.gpu = gpu.1.clone();

        if events.send(Event::Stats(stats)).await.is_err() {
            return;
        }
    }
}

fn read(path: &str) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

/// (total, idle) jiffies for the `cpu` line, then each `cpuN` line.
fn parse_proc_stat(text: &str) -> Vec<(u64, u64)> {
    text.lines()
        .filter(|l| l.starts_with("cpu"))
        .map(|l| {
            let fields: Vec<u64> = l.split_whitespace().skip(1).filter_map(|f| f.parse().ok()).collect();
            // idle + iowait count as idle.
            let idle = fields.get(3).copied().unwrap_or(0) + fields.get(4).copied().unwrap_or(0);
            (fields.iter().sum(), idle)
        })
        .collect()
}

fn busy_percent(prev: (u64, u64), cur: (u64, u64)) -> u8 {
    let total = cur.0.saturating_sub(prev.0);
    let idle = cur.1.saturating_sub(prev.1);
    if total == 0 {
        return 0;
    }
    (total.saturating_sub(idle) * 100 / total).min(100) as u8
}

/// At most `MAX_CORES` bars: larger CPUs are averaged in adjacent groups.
fn fold_cores(cores: &[u8]) -> Vec<u8> {
    if cores.len() <= MAX_CORES {
        return cores.to_vec();
    }
    let group = cores.len().div_ceil(MAX_CORES);
    cores.chunks(group).map(|c| (c.iter().map(|&v| u32::from(v)).sum::<u32>() / c.len() as u32) as u8).collect()
}

/// (MemTotal, MemAvailable) in KiB.
fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let field = |name: &str| {
        text.lines().find(|l| l.starts_with(name))?.split_whitespace().nth(1)?.parse::<u64>().ok()
    };
    Some((field("MemTotal:")?, field("MemAvailable:")?))
}

/// Total (rx, tx) bytes over every interface but loopback.
fn parse_net_dev(text: &str) -> (u64, u64) {
    text.lines()
        .skip(2)
        .filter_map(|l| {
            let (name, rest) = l.split_once(':')?;
            if name.trim() == "lo" {
                return None;
            }
            let f: Vec<u64> = rest.split_whitespace().filter_map(|v| v.parse().ok()).collect();
            Some((*f.first()?, *f.get(8)?))
        })
        .fold((0, 0), |(r, t), (rx, tx)| (r + rx, t + tx))
}

/// The CPU package sensor: AMD `k10temp`/`zenpower` or Intel `coretemp`.
fn cpu_temp_path() -> Option<PathBuf> {
    std::fs::read_dir("/sys/class/hwmon").ok()?.flatten().map(|e| e.path()).find_map(|dir| {
        let name = std::fs::read_to_string(dir.join("name")).ok()?;
        matches!(name.trim(), "k10temp" | "zenpower" | "coretemp").then(|| dir.join("temp1_input"))
    })
}

async fn read_gpu() -> Option<Gpu> {
    let query = tokio::process::Command::new("nvidia-smi")
        .args(["--query-gpu=utilization.gpu,temperature.gpu,memory.used,memory.total", "--format=csv,noheader,nounits"])
        // A hung nvidia-smi must die with the timeout, not pile up one per
        // sample.
        .kill_on_drop(true)
        .output();
    let out = tokio::time::timeout(GPU_TIMEOUT, query).await.ok()?.ok()?;
    out.status.success().then(|| parse_nvidia(&String::from_utf8_lossy(&out.stdout))).flatten()
}

/// The first GPU's line of `nvidia-smi --format=csv,noheader,nounits`.
fn parse_nvidia(text: &str) -> Option<Gpu> {
    let f: Vec<&str> = text.lines().next()?.split(',').map(str::trim).collect();
    let num = |i: usize| f.get(i).and_then(|v| v.parse::<u32>().ok());
    Some(Gpu {
        util: num(0).map(|v| v.min(100) as u8),
        temp: num(1).map(|v| v as i16),
        vram_used: num(2),
        vram_total: num(3),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpu_busy_from_jiffy_deltas() {
        let a = parse_proc_stat("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 50 0 50 350 50 0 0 0 0 0\nintr 1\n");
        let b = parse_proc_stat("cpu  200 0 200 1300 300 0 0 0 0 0\ncpu0 100 0 100 650 150 0 0 0 0 0\n");
        assert_eq!(a.len(), 2);
        // total +1000, idle(+600 idle +200 iowait) → 20 % busy.
        assert_eq!(busy_percent(a[0], b[0]), 20);
        assert_eq!(busy_percent(a[0], a[0]), 0, "no time passed");
    }

    #[test]
    fn many_cores_fold_into_the_bar_limit() {
        assert_eq!(fold_cores(&[10; 12]).len(), 12);
        let folded = fold_cores(&(0..32).map(|i| i as u8).collect::<Vec<_>>());
        assert_eq!(folded.len(), MAX_CORES);
        assert_eq!(folded[0], 0); // (0 + 1) / 2
        assert_eq!(folded[15], 30); // (30 + 31) / 2
    }

    #[test]
    fn meminfo_and_net_dev() {
        let mem = "MemTotal:       65761728 kB\nMemFree: 1 kB\nMemAvailable:   56947012 kB\n";
        assert_eq!(parse_meminfo(mem), Some((65761728, 56947012)));
        assert_eq!(parse_meminfo("MemTotal: 1 kB\n"), None);

        let net = "Inter-|   Receive\n face |bytes\n    lo: 999 1 0 0 0 0 0 0 999 1 0 0 0 0 0 0\n  eth0: 100 1 0 0 0 0 0 0 40 1 0 0 0 0 0 0\n wlan0: 5 1 0 0 0 0 0 0 2 1 0 0 0 0 0 0\n";
        assert_eq!(parse_net_dev(net), (105, 42));
    }

    #[test]
    fn nvidia_csv() {
        let gpu = parse_nvidia("20, 42, 1731, 8192\n").unwrap();
        assert_eq!((gpu.util, gpu.temp, gpu.vram_used, gpu.vram_total), (Some(20), Some(42), Some(1731), Some(8192)));
        let partial = parse_nvidia("[N/A], 42, 1731, 8192\n").unwrap();
        assert_eq!(partial.util, None, "an unreadable field is missing, not zero");
    }
}
