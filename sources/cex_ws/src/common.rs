use std::fs::File;
use std::io::{BufWriter, Write};
use std::os::fd::RawFd;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

pub type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

/// Spin this long before each slot; tokio's timer has 1 ms resolution and can oversleep by a few ms
const SPIN: Duration = Duration::from_millis(5);

#[derive(clap::Args)]
pub struct RunArgs {
    /// Wall-clock start as Unix ms, shared by all instances so their slots line up [default: 2 s from now]
    #[arg(long)]
    pub start_at: Option<u64>,
    /// Write every sample to this CSV file
    #[arg(long)]
    pub out: Option<PathBuf>,
}

pub fn wall_us() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before 1970")
        .as_micros() as u64
}

/// Fixed time grid: slot k starts at start + k * interval on every instance (clocks synced by chrony).
pub struct Grid {
    start: Instant,
    start_wall_us: u64,
    interval: Duration,
}

impl Grid {
    pub fn new(start_at_ms: Option<u64>, interval: Duration) -> Self {
        let (now, now_wall) = (Instant::now(), wall_us());
        let start_wall_us = start_at_ms.map_or(now_wall + 2_000_000, |ms| ms * 1000);
        let start = if start_wall_us >= now_wall {
            now + Duration::from_micros(start_wall_us - now_wall)
        } else {
            let behind = Duration::from_micros(now_wall - start_wall_us);
            eprintln!(
                "Warning: --start-at is {} ms in the past; early slots are skipped",
                behind.as_millis()
            );
            now.checked_sub(behind).unwrap_or(now)
        };
        Grid {
            start,
            start_wall_us,
            interval,
        }
    }

    pub fn deadline(&self, slot: u64) -> Instant {
        self.start + Duration::from_nanos(self.interval.as_nanos() as u64 * slot)
    }

    /// Microseconds from the slot's grid time to now (positive = late)
    pub fn offset_us(&self, slot: u64) -> i64 {
        wall_us() as i64 - (self.start_wall_us + self.interval.as_micros() as u64 * slot) as i64
    }

    /// Sleeps to just before the slot, then spins onto it. Returns false if already half an interval late.
    pub async fn wait(&self, slot: u64) -> bool {
        let deadline = self.deadline(slot);
        tokio::time::sleep_until(deadline.checked_sub(SPIN).unwrap_or(deadline).into()).await;
        while Instant::now() < deadline {
            std::hint::spin_loop();
        }
        Instant::now().saturating_duration_since(deadline) <= self.interval / 2
    }

    pub async fn sleep_until(&self, slot: u64) {
        tokio::time::sleep_until(self.deadline(slot).into()).await;
    }
}

/// "n=… min=… p50=… … max=…" in µs; a percentile is shown only with at least 5 samples above it
pub fn stats(values: &[u64]) -> String {
    if values.is_empty() {
        return "n=0".into();
    }
    let mut v = values.to_vec();
    v.sort_unstable();
    let n = v.len();
    let mut s = format!("n={n} min={}", v[0]);
    for (label, p) in [
        ("p50", 0.50),
        ("p90", 0.90),
        ("p99", 0.99),
        ("p99.9", 0.999),
    ] {
        if n as f64 * (1.0 - p) >= 5.0 {
            s += &format!(" {label}={}", v[((n - 1) as f64 * p).round() as usize]);
        }
    }
    s + &format!(" max={}", v[n - 1])
}

pub fn write_csv(path: &Path, header: &str, rows: impl IntoIterator<Item = String>) -> Result<()> {
    let mut w = BufWriter::new(File::create(path)?);
    writeln!(w, "{header}")?;
    for row in rows {
        writeln!(w, "{row}")?;
    }
    w.flush()?;
    Ok(())
}

pub struct TcpInfo {
    pub rtt_us: u32,
    pub total_retrans: u32,
}

#[cfg(target_os = "linux")]
pub fn tcp_info(fd: RawFd) -> Option<TcpInfo> {
    // SAFETY: tcp_info is plain old data and getsockopt writes at most `len` bytes into it
    let mut info: libc::tcp_info = unsafe { std::mem::zeroed() };
    let mut len = std::mem::size_of::<libc::tcp_info>() as libc::socklen_t;
    let rc = unsafe {
        libc::getsockopt(
            fd,
            libc::IPPROTO_TCP,
            libc::TCP_INFO,
            (&raw mut info).cast(),
            &mut len,
        )
    };
    (rc == 0).then_some(TcpInfo {
        rtt_us: info.tcpi_rtt,
        total_retrans: info.tcpi_total_retrans,
    })
}

#[cfg(not(target_os = "linux"))]
pub fn tcp_info(_fd: RawFd) -> Option<TcpInfo> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stats_hides_percentiles_without_enough_samples_above() {
        let values: Vec<u64> = (1..=100).collect();
        assert_eq!(stats(&values), "n=100 min=1 p50=51 p90=90 max=100");
        assert_eq!(stats(&[]), "n=0");
    }
}
