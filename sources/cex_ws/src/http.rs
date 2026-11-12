use std::net::{IpAddr, SocketAddr};
use std::os::fd::{AsRawFd, RawFd};
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::client::conn::http1::{self, SendRequest};
use hyper::{Method, Request};
use hyper_util::rt::TokioIo;
use rustls::pki_types::ServerName;
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use url::Url;

use crate::common::{self, Grid, Layout, Result, RunArgs};

const TIMEOUT: Duration = Duration::from_secs(2);

#[derive(clap::Args)]
pub struct HttpArgs {
    /// Target IP; TCP goes straight there while TLS SNI and Host come from --url
    target_ip: IpAddr,
    /// Request URL, e.g. https://fapi.binance.com/fapi/v1/time
    #[arg(long)]
    url: Url,
    #[arg(long, default_value = "GET")]
    method: Method,
    /// Request body; {id} is replaced by the slot number
    #[arg(long)]
    body: Option<String>,
    /// Extra request header as "Name: value"; repeatable
    #[arg(long = "header")]
    headers: Vec<String>,
    /// Measured responses in total, split evenly over the connections
    #[arg(long, default_value_t = 600)]
    samples: u64,
    /// Sequential keep-alive connections; each may land on a different backend behind the target IP
    #[arg(long, default_value_t = 3)]
    connections: u64,
    /// Responses discarded at the start of each connection
    #[arg(long, default_value_t = 5)]
    warmup: u64,
    #[arg(long, default_value_t = 100)]
    interval_ms: u64,
    #[command(flatten)]
    run: RunArgs,
}

struct Sample {
    slot: u64,
    conn: u64,
    warmup: bool,
    status: &'static str,
    offset_us: i64,
    rtt_us: u64,
    srtt_us: u64,
    http_status: u16,
}

/// Measures HTTP request round trips on keep-alive connections: request write to parsed response head.
pub async fn run(args: HttpArgs) -> Result<()> {
    let host = args.url.host_str().ok_or("--url needs a host")?.to_string();
    let host_header = match args.url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host.clone(),
    };
    let connector = match args.url.scheme() {
        "https" => Some(tls_connector()),
        "http" => None,
        other => return Err(format!("unsupported scheme {other}; use http or https").into()),
    };
    if args.samples == 0 || args.connections == 0 || args.interval_ms == 0 {
        return Err("--samples, --connections and --interval-ms must be positive".into());
    }
    let path = match args.url.query() {
        Some(q) => format!("{}?{q}", args.url.path()),
        None => args.url.path().to_string(),
    };
    let headers = args
        .headers
        .iter()
        .map(|h| {
            h.split_once(':')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect::<Option<Vec<_>>>()
        .ok_or("--header must look like \"Name: value\"")?;
    let addr = SocketAddr::new(args.target_ip, args.url.port_or_known_default().unwrap());
    let layout = Layout::new(
        args.interval_ms,
        args.warmup,
        args.samples,
        args.connections,
    );
    let grid = Grid::new(args.run.start_at, Duration::from_millis(args.interval_ms));

    let mut samples = Vec::new();
    let mut connects = Vec::new();
    for c in 0..layout.connections {
        let (first, end) = layout.requests(c);
        grid.sleep_until(c * layout.period()).await;
        let sample = |slot, status, offset_us, rtt_us, srtt_us, http_status| Sample {
            slot,
            conn: c,
            warmup: layout.is_warmup(slot),
            status,
            offset_us,
            rtt_us,
            srtt_us,
            http_status,
        };
        let (mut sender, fd, tcp_connect) = match connect(addr, &host, connector.as_ref()).await {
            Ok(connected) => connected,
            Err(e) => {
                eprintln!("connection {c}: {e}");
                samples.extend((first..end).map(|slot| sample(slot, "noconn", 0, 0, 0, 0)));
                continue;
            }
        };
        connects.push(tcp_connect.as_micros() as u64);

        let mut next = first;
        while next < end {
            let slot = next;
            next += 1;
            if !grid.wait(slot).await {
                samples.push(sample(slot, "late", 0, 0, 0, 0));
                continue;
            }
            let offset = grid.offset_us(slot);
            let mut req = Request::builder()
                .method(args.method.clone())
                .uri(path.as_str())
                .header(hyper::header::HOST, &host_header);
            for (k, v) in &headers {
                req = req.header(k, v);
            }
            let body = args
                .body
                .as_ref()
                .map(|b| b.replace("{id}", &slot.to_string()))
                .unwrap_or_default();
            let req = req.body(Full::new(Bytes::from(body)))?;
            let sent = Instant::now();
            match tokio::time::timeout(TIMEOUT, sender.send_request(req)).await {
                Ok(Ok(resp)) => {
                    let rtt_us = sent.elapsed().as_micros() as u64;
                    let http_status = resp.status().as_u16();
                    // Drain the body so the connection can carry the next request
                    let _ = resp.into_body().collect().await;
                    let srtt = common::tcp_info(fd).map_or(0, |i| i.rtt_us as u64);
                    samples.push(sample(slot, "ok", offset, rtt_us, srtt, http_status));
                }
                Ok(Err(e)) => {
                    eprintln!("connection {c}: {e}");
                    samples.push(sample(slot, "error", offset, 0, 0, 0));
                    break;
                }
                Err(_) => {
                    samples.push(sample(slot, "timeout", offset, 0, 0, 0));
                    break;
                }
            }
        }
        samples.extend((next..end).map(|slot| sample(slot, "noconn", 0, 0, 0, 0)));
    }

    let ok = |s: &&Sample| s.status == "ok" && !s.warmup;
    println!("HTTP round trip in µs (request write to parsed response head):");
    println!(
        "all: {}",
        common::stats(
            &samples
                .iter()
                .filter(ok)
                .map(|s| s.rtt_us)
                .collect::<Vec<_>>()
        )
    );
    for (c, tcp_connect) in connects.iter().enumerate() {
        let rtt: Vec<u64> = samples
            .iter()
            .filter(ok)
            .filter(|s| s.conn == c as u64)
            .map(|s| s.rtt_us)
            .collect();
        println!(
            "connection {c}: {} tcp_connect={tcp_connect}",
            common::stats(&rtt)
        );
    }
    let not_2xx = samples
        .iter()
        .filter(|s| s.status == "ok" && !(200..300).contains(&s.http_status))
        .count();
    if not_2xx > 0 {
        println!("non-2xx responses: {not_2xx}");
    }
    for status in ["error", "timeout", "late", "noconn"] {
        let n = samples.iter().filter(|s| s.status == status).count();
        if n > 0 {
            println!("{status}: {n}");
        }
    }

    if let Some(path) = &args.run.out {
        let rows = samples.iter().map(|s| {
            format!(
                "{},{},{},{},{},{},{},{}",
                s.slot,
                s.conn,
                s.warmup as u8,
                s.status,
                s.offset_us,
                s.rtt_us,
                s.srtt_us,
                s.http_status
            )
        });
        common::write_csv(
            path,
            "slot,conn,warmup,status,offset_us,rtt_us,srtt_us,http_status",
            rows,
        )?;
    }
    Ok(())
}

/// Opens TCP straight to `addr`, then TLS with `host` as SNI when a connector is given, then an HTTP/1.1 connection.
async fn connect(
    addr: SocketAddr,
    host: &str,
    tls: Option<&TlsConnector>,
) -> Result<(SendRequest<Full<Bytes>>, RawFd, Duration)> {
    let start = Instant::now();
    let tcp = TcpStream::connect(addr).await?;
    let tcp_connect = start.elapsed();
    tcp.set_nodelay(true)?;
    let fd = tcp.as_raw_fd();
    let sender = match tls {
        Some(tls) => {
            let stream = tls
                .connect(ServerName::try_from(host.to_string())?, tcp)
                .await?;
            let (sender, conn) = http1::handshake(TokioIo::new(stream)).await?;
            tokio::spawn(conn);
            sender
        }
        None => {
            let (sender, conn) = http1::handshake(TokioIo::new(tcp)).await?;
            tokio::spawn(conn);
            sender
        }
    };
    Ok((sender, fd, tcp_connect))
}

fn tls_connector() -> TlsConnector {
    let mut roots = rustls::RootCertStore::empty();
    for cert in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(cert);
    }
    let mut config = rustls::ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    TlsConnector::from(Arc::new(config))
}
