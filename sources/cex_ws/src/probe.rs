use std::net::{IpAddr, SocketAddr};
use std::os::fd::AsRawFd;
use std::str::FromStr;
use std::time::{Duration, Instant};

use tokio::net::TcpStream;

use crate::common::{self, Grid, Result, RunArgs};

const TIMEOUT: Duration = Duration::from_millis(500);

#[derive(clap::Args)]
pub struct ProbeArgs {
    /// Targets as IP or IP:port, comma separated; give every instance the same list in the same order so slots pair up
    #[arg(long, value_delimiter = ',', required = true)]
    targets: Vec<Target>,
    /// Port for targets given without one
    #[arg(long, default_value_t = 443)]
    port: u16,
    /// Measured handshakes per target
    #[arg(long, default_value_t = 200)]
    count: u64,
    /// Handshakes per target discarded first (ARP, route and connection-tracking setup)
    #[arg(long, default_value_t = 2)]
    warmup: u64,
    /// Slot length; one handshake per slot, cycling through the targets
    #[arg(long, default_value_t = 25)]
    interval_ms: u64,
    #[command(flatten)]
    run: RunArgs,
}

#[derive(Clone)]
pub struct Target {
    ip: IpAddr,
    port: Option<u16>,
}

impl FromStr for Target {
    type Err = String;

    fn from_str(s: &str) -> std::result::Result<Self, String> {
        if let Ok(addr) = s.parse::<SocketAddr>() {
            return Ok(Target {
                ip: addr.ip(),
                port: Some(addr.port()),
            });
        }
        s.parse()
            .map(|ip| Target { ip, port: None })
            .map_err(|_| format!("invalid target {s}: expected IP or IP:port"))
    }
}

struct Sample {
    slot: u64,
    target: SocketAddr,
    warmup: bool,
    status: &'static str,
    offset_us: i64,
    connect_us: u64,
    syn_rtt_us: u64,
}

/// Measures TCP handshake RTT, the network-only part of the path: the peer's kernel answers the SYN.
pub async fn run(args: ProbeArgs) -> Result<()> {
    if args.count == 0 || args.interval_ms == 0 {
        return Err("--count and --interval-ms must be positive".into());
    }
    let targets: Vec<SocketAddr> = args
        .targets
        .iter()
        .map(|t| SocketAddr::new(t.ip, t.port.unwrap_or(args.port)))
        .collect();
    let n_targets = targets.len() as u64;
    let total = (args.warmup + args.count) * n_targets;
    let grid = Grid::new(args.run.start_at, Duration::from_millis(args.interval_ms));
    eprintln!(
        "probe: {n_targets} targets x {} handshakes in {total} slots of {} ms (~{:.0} s)",
        args.warmup + args.count,
        args.interval_ms,
        (total * args.interval_ms) as f64 / 1000.0
    );

    let mut samples = Vec::with_capacity(total as usize);
    for slot in 0..total {
        let target = targets[(slot % n_targets) as usize];
        let mut s = Sample {
            slot,
            target,
            warmup: slot / n_targets < args.warmup,
            status: "late",
            offset_us: 0,
            connect_us: 0,
            syn_rtt_us: 0,
        };
        if grid.wait(slot).await {
            s.offset_us = grid.offset_us(slot);
            let start = Instant::now();
            match tokio::time::timeout(TIMEOUT, TcpStream::connect(target)).await {
                Ok(Ok(stream)) => {
                    s.connect_us = start.elapsed().as_micros() as u64;
                    // The kernel's first RTT sample is the SYN/SYN-ACK exchange, free of user-space wakeup delay
                    s.syn_rtt_us =
                        common::tcp_info(stream.as_raw_fd()).map_or(0, |i| i.rtt_us as u64);
                    s.status = "ok";
                }
                Ok(Err(_)) => s.status = "error",
                Err(_) => s.status = "timeout",
            }
        }
        samples.push(s);
    }

    println!(
        "TCP handshake RTT per target in µs (kernel SYN/SYN-ACK sample; connect() time in brackets):"
    );
    let mut all = Vec::new();
    for target in &targets {
        let measured: Vec<&Sample> = samples
            .iter()
            .filter(|s| s.target == *target && !s.warmup)
            .collect();
        let ok: Vec<&&Sample> = measured.iter().filter(|s| s.status == "ok").collect();
        let syn: Vec<u64> = ok.iter().map(|s| s.syn_rtt_us).filter(|&v| v > 0).collect();
        let connect: Vec<u64> = ok.iter().map(|s| s.connect_us).collect();
        all.extend(&syn);
        println!(
            "{target}: {} [{}] failed={}",
            common::stats(&syn),
            common::stats(&connect),
            measured.len() - ok.len()
        );
    }
    println!("all targets: {}", common::stats(&all));

    if let Some(path) = &args.run.out {
        let rows = samples.iter().map(|s| {
            format!(
                "{},{},{},{},{},{},{}",
                s.slot, s.target, s.warmup as u8, s.status, s.offset_us, s.connect_us, s.syn_rtt_us
            )
        });
        common::write_csv(
            path,
            "slot,target,warmup,status,offset_us,connect_us,syn_rtt_us",
            rows,
        )?;
    }
    Ok(())
}
