# CEX WebSocket EC2 Hunting

EC2 Hunting for CEX WebSocket API latency using AWS EC2 instances with partition placement groups.

## Overview

This project launches a fleet of EC2 instances (default 7 × `c8i.2xlarge`) in a partition placement group, measures every instance's latency to a crypto exchange's endpoints at the same moments, ranks the instances, and keeps the winner as the production host.

Measurement follows two rules: rank by a network-only metric that uses the same protocol as real traffic (TCP handshake RTT), and collect every sample centrally for offline percentile, histogram and paired analysis. Application-level round trips and market-data arrival are measured as well, to confirm the ranking on the paths that matter.

### The Hunting Path
![](assets/hunting-path.png)

### Architecture

- **EC2 Instances**: `instance_count` × `instance_type` (default 7 × `c8i.2xlarge`), spread round-robin over the 7 partitions, plus optional `unplaced_instance_count` instances outside the placement group
- **Placement Strategy**: Partition placement group; see [What the placement group gives you](#what-the-placement-group-gives-you)
- **AMI**: your production AMI via `ami_id`, otherwise Amazon Linux 2023 (kernel 6.18)
- **Region**: ap-northeast-1 (Tokyo)
- **Performance Optimizations** (`scripts/client.sh`, logged at every boot with a `HUNT:` prefix):
  - Hyper-threading disabled: `cpu_options` with the type's core count and `threads_per_core = 1`, backed by `nosmt` and an SMT check in `client.sh`
  - CPU isolation: cores 2..N-1 isolated via kernel boot parameters (`isolcpus`, `nohz_full`, `rcu_nocbs`), set on first boot followed by one reboot; the benchmark is pinned to them
  - Housekeeping: cores 0-1 take all device interrupts (`irqaffinity`, irqbalance disabled), unbound kernel workqueues and system services
  - Noise reduction: `idle=poll`, `clocksource=tsc`, NMI watchdog, audit and THP off, socket busy polling, NIC interrupt moderation off
  - Turbo Boost and CPU governor: set only where the platform exposes them, which on EC2 means bare metal; elsewhere frequency is managed by the platform

> **Note**: The goal is measurement consistency — ensuring observed latency differences come from network path placement, not host noise such as scheduler, interrupt or idle-state jitter.

## Prerequisites

- Terraform >= 1.7
- AWS CLI v2 with credentials for the target account; [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html) for interactive sessions
- Python 3.11+ for `scripts/hunt.py`
- An existing VPC and S3 bucket in the region
- A Running On-Demand vCPU quota well above the fleet size: hunting launches every instance at once
- Rust toolchain (for local testing only)

The client and default AMI are x86_64 only, and the tuning in `client.sh` targets Amazon Linux 2023.

## Local Testing

```bash
cd sources/cex_ws
dig +short <ws-fapi.cex-endpoint.com>            # candidate IPs

cargo run --release -- probe --targets <ip1>,<ip2> --count 20
cargo run --release -- ws <ip> --url wss://<ws-fapi.cex-endpoint.com>/ws-fapi/v1 --samples 10 --connections 2 --interval-ms 200
cargo run --release -- stream <ip> --url wss://<fstream.cex-endpoint.com>/ws/btcusdt@bookTicker --seconds 5
cargo run --release -- http <ip> --url https://<fapi.cex-endpoint.com>/fapi/v1/time --samples 10 --interval-ms 200
```

Kernel TCP figures are reported on Linux only. Behind a local proxy or VPN, TCP figures only cover the hop to the proxy; measure on EC2.

## Deployment

### 1. Package Source Code

```bash
sh pack_source.sh
```

This creates `sources/source.tar.gz` containing the Rust project.

### 2. Configure Variables

```bash
cp terraform.tfvars.example terraform.tfvars
```

Edit `terraform.tfvars`:
- `aws_vpc_id`: your target VPC ID
- `aws_availability_zone_id`: the AZ **ID** (e.g. `apne1-az4`), not the AZ name; names map to different AZs in every account. Use the exchange's AZ ID
- `s3_bucket`: S3 bucket for the client source and the results
- `internet_gateway_id` (optional): the VPC's existing Internet Gateway; required when the VPC already has one, since a VPC takes only one
- `instance_type`, `instance_count` (optional): the production type and size, and how many placements to sample (default 7 × `c8i.2xlarge`)
- `unplaced_instance_count` (optional): extra instances outside the placement group
- `ami_id` (optional): the AMI to hunt with, see below

See `variables.tf` for all available options.

#### Hunt with your production instance type and AMI

Set `instance_type` and `ami_id` to what the winning instance will run in production (e.g. your latency-tuned golden AMI). An instance keeps its host across reboots, but changing its type or size needs a stop/start and switching AMI means launching a new instance; both move it to a new host and throw the hunted placement away. Without `ami_id`, the latest Amazon Linux 2023 AMI with kernel 6.18 is used.

- The AMI must be x86_64 with cloud-init, AWS CLI v2 (to fetch the source from S3) and SSM Agent (the only access path). `scripts/client.sh` is written for Amazon Linux 2023; on other distributions its tuning steps may fail.
- Existing instances ignore later changes to `instance_type`, `ami_id`, the default AMI or placement settings. To re-hunt with a new type or AMI, run `terraform apply -replace='aws_instance.client_ppg["N"]'` per instance, or destroy and apply again.
- If the AMI already sets `isolcpus`, `client.sh` leaves the kernel command line unchanged and the benchmark runs on the AMI's isolated cores.

### 3. Deploy Infrastructure

```bash
terraform init
terraform apply --auto-approve
```

This provisions:
- Partition placement group with 7 partitions
- VPC subnet, internet gateway (unless `internet_gateway_id` is set), and routing
- Security group (egress-only, no inbound ports)
- S3 VPC endpoint
- IAM role and instance profile (read the source, write `results/` in the bucket, SSM)
- EC2 instances with cloud-init scripts

On every boot each instance runs `scripts/client.sh`, which:
- applies the host tuning above; on first boot it sets the CPU isolation boot parameters and reboots once
- downloads the Rust source from S3 and builds it when the source changed

Wait until `grep HUNT: /var/log/cloud-init-output.log` shows `cmdline: active` (or `the AMI sets its own isolcpus`) before measuring; this takes about 2 minutes on `c8i.2xlarge`.

### 4. Measure

`scripts/hunt.py run` starts `cex_ws` on every instance via SSM with one shared start time, waits for it, gathers each instance's samples, clock status and metadata under `s3://<bucket>/results/<run-id>/<instance-id>/`, downloads them to `results/<run-id>/` and prints the ranking. The exchange and its endpoints come from a [target profile](#target-profiles); arguments after `--` go to `cex_ws`.

```bash
P=targets/binance-usdm.toml

# 1. Rank by network-only RTT to the IPs behind every endpoint of the profile
scripts/hunt.py run --profile $P -- --count 200 --interval-ms 50

# 2. Confirm on each endpoint's own protocol, over the IP you will pin in production
scripts/hunt.py run --profile $P --endpoint ws-api --ip <ip> -- --samples 1200 --connections 6
scripts/hunt.py run --profile $P --endpoint rest --ip <ip>

# 3. Record the market-data stream on every host (reported, not ranked; see the clock rule below)
scripts/hunt.py run --profile $P --endpoint market-stream -- --seconds 60

# Repeat at different times of day, then check that the winner holds across runs
scripts/hunt.py compare results/<run-1> results/<run-2> results/<run-3>
```

Without a profile, give the mode and every `cex_ws` argument yourself, e.g. `scripts/hunt.py run probe --hosts ws-fapi.binance.com` or `scripts/hunt.py run ws -- <ip> --url wss://...`.

Hostnames are resolved on an instance, both through the VPC resolver and by asking the zone's authoritative name servers directly: DNS answers can depend on where the query comes from (from outside AWS, `fapi.binance.com` may resolve to CloudFront), each answer is a rotating subset, and the VPC resolver caches one subset for the TTL. Some hosts front hundreds of IPs (Binance `fstream`: ~470), so a probe takes a fixed-seed sample of `--max-targets-per-host` (16) per host, and refuses to start if it would run longer than `--max-minutes` (10).

Reading the report:
- **median Δ vs fleet**: for every slot (probe, ws) or event (stream), each instance's value minus the fleet median at that same moment, then the median per instance with a bootstrap 95% confidence interval. Pairing by time cancels load swings on the exchange side, which are often larger than the differences between placements.
- **verdict**: the winner versus the runner-up on their paired differences. A lead counts only if its confidence interval excludes zero and it is at least `--min-diff-us` (default 5 µs). An unresolved lead comes with the number of paired samples that would resolve it.
- **distribution** and **histogram**: percentiles (shown only with at least 5 samples above them) and one sparkline per instance over shared bins.
- **per target** (probe): the best instance for each IP; a hostname's IPs can rank instances differently, so pin production connections to the IP you hunted for.
- **per role** (probe with a profile): separate rankings for order entry (`order`) and market data (`md`) targets, their rank correlation (near +1: the same hosts are close to both), a score weighted by the profile's `[weights]` (or `compare --weights order=0.7,md=0.3`), and the suggested `keep_instances` for one host that does both.
- `results/<run-id>/combined.csv` holds every sample of every instance for further offline analysis.

A winner is credible when it is significant in each run and stays first across runs taken at different times.

**How many samples.** The confidence interval's half-width shrinks with the square root of the sample count. In a Binance run on `c8i.2xlarge` (paired differences between two instances with a standard deviation of ~140 µs) it was ±19 µs at 100 paired samples, ±11 µs at 300, ±6 µs at 1,000 and ±3.5 µs at 3,000. Placements differed by 10–15 µs, which needs a half-width of about ±5 µs: some 1,500 paired samples for the instance ranking, counted over all target IPs together (e.g. 30 IPs × 50). Choosing which IP to pin needs per-IP samples; ~300 per IP resolves ±10 µs. Repeating a run at other times of day says more about a placement than one longer run.

### 5. Keep the Winner

Find the index of the best instance (`terraform output instances` maps index to instance ID and partition), then keep only it:

```hcl
# terraform.tfvars
keep_instances = [3]
```

```bash
terraform plan    # must only destroy the other instances
terraform apply
```

To protect the winner's placement:
- Only reboot it. Stop/start, changing instance type, size, CPU options, user data, placement group or tenancy moves it to another host, so hunt with the production instance type, size and AMI.
- Later edits to `instance_type`, `ami_id` or `scripts/` are ignored for existing instances, so re-applying doesn't replace them; they apply to newly launched instances only.
- To update the Rust client on running instances, run `sh pack_source.sh` and `terraform apply`, then reboot them: the boot script rebuilds when the source changed.
- `terraform destroy` removes everything, including the winner.
- Re-hunt periodically, as exchange endpoints and AWS capacity change over time: remove `keep_instances` and apply to launch fresh instances alongside the winner.

### 6. Destroy Infrastructure

```bash
terraform destroy --auto-approve
```

## Measurement Modes

All modes share `--start-at <unix-ms>` (a time grid common to all instances; `hunt.py` sets it) and `--out <csv>` (every sample). Sends are scheduled on the grid and spun onto it, so instances hit the exchange together, within their clock-sync error (tens of µs); each sample records its offset from the grid.

| Mode | Measures | Key options |
|---|---|---|
| `probe` | TCP handshake RTT: the kernel's RTT sample from SYN/SYN-ACK (`TCP_INFO`), free of user-space wakeup delay; `connect()` time as fallback. Targets are probed round-robin, one handshake per slot, each on a new connection. Works for any TCP service (HTTPS, WebSocket, FIX) | `--targets` (`IP` or `IP:port`, `--port` default 443), `--count` (default 200 per target), `--warmup` (2, discards ARP/route/conntrack setup), `--interval-ms` (25) |
| `ws` | App round trip on established WebSocket connections: from just before the request write to the moment the response frame is read, before JSON parsing. Also kernel srtt and retransmits | `--url`, `--request` (JSON with `{id}`, replaced by the slot number; default Binance-style `depth`), `--id-field` (top-level response field echoing it, string or number; `id`), `--samples` (600 in total), `--connections` (3, sequential), `--warmup` (5 per connection), `--interval-ms` (100) |
| `http` | HTTP request round trip on keep-alive connections: from the request write to the parsed response head. TCP goes straight to the target IP; TLS SNI and `Host` come from the URL | `--url`, `--method` (GET), `--body` (with `{id}`), `--header` (repeatable), `--samples`, `--connections`, `--warmup`, `--interval-ms` as for `ws` |
| `stream` | Wall-clock arrival time of each stream event, with the exchange's event time for a millisecond-level sanity check; reported per instance, not ranked (see the clock rule below) | `--url`, `--subscribe`, `--key` (`u`, Binance bookTicker update ID), `--event-time` (`E`), `--seconds` (30 per connection), `--connections` (2) |

What the numbers mean and where they stop:
- **Who answers the SYN.** Probe RTT ends wherever TCP terminates. For IPs that are the exchange's own servers or a pass-through load balancer, that is the path to the exchange; for IPs behind a CDN or proxying load balancer (CloudFront, ALB), it only reaches the edge, and the ranking says nothing about the backend.
- **App round trip includes exchange processing.** On Binance ws-fapi from `c8i.2xlarge` in apne1-az4, the TCP handshake takes ~0.2 ms, the `depth` round trip ~0.9 ms; most of the difference is exchange time, and `depth` is likely answered by a gateway cache rather than the matching engine. The kernel's srtt tracks the app round trip because the exchange piggybacks its ACK on the response, so it is not network-only either.
- **Several connections.** A target IP can front several gateways; each connection lands on one. `--connections` and the per-connection p50s in the report show whether results depend on the backend rather than on placement.
- **Clock rule: rank only on same-machine time differences.** All instances start together on wall-clock time, synced by the basic Amazon Time Sync Service (`169.254.169.123`; chrony error bounds of 70–90 µs were measured, and `client.sh` logs the source and error terms at every boot). That aligns sending slots well enough, but not arrival times compared across machines, where placements differ by less than the clock error. So probe, ws and http rank on round trips timed on one machine, and stream runs are reported without a ranking; distance to the market-data servers comes from the probe RTT to their IPs (role `md`). Hardware clocks (PHC) and NIC hardware timestamps would remove the clock error and the host stack's noise, but need a precision-time placement group, which a partition placement group cannot be combined with.
- **Not measured: the order path.** Order round trips need signed requests on a funded account and real orders; testnets run on different infrastructure, so they say nothing about placement.
- **Exchange etiquette.** Use documented endpoints and ports only, and stay within the exchange's published limits per source IP (each instance has its own). Binance USDⓈ-M futures, for example: REST and WebSocket API share a request-weight budget of 2,400 per minute per IP, each WebSocket handshake costs 5, a `depth` request with `limit` 5 costs 2, so the default `ws` run (one request per 100 ms) uses about half of it; market-stream connections accept at most 10 incoming messages per second, and IPs repeatedly disconnected for exceeding limits may be banned. Exchanges don't publish limits for bare TCP handshakes, so keep probes gentle: the defaults open 40 connections per second per instance (`--interval-ms 50` halves it), and `--max-targets-per-host` and `--max-minutes` keep runs bounded.

## Target Profiles

A profile in `targets/` describes one exchange: its endpoints and how to measure each. `targets/binance-usdm.toml` is a worked example; copy it for another exchange.

```toml
name = "my-exchange"

[[endpoints]]
name = "ws-api"              # used by --endpoint and in reports
protocol = "ws"              # tcp | ws | http | stream
url = "wss://ws.example.com/v1"
request = '{"id":"{id}","op":"ping"}'
id_field = "id"

[[endpoints]]
name = "md"
protocol = "stream"
url = "wss://stream.example.com/ws"
subscribe = '{"op":"subscribe","args":["book.BTCUSDT"]}'
key_field = "seq"            # same value on every instance for the same event
event_time_field = "ts"

[[endpoints]]
name = "rest"
protocol = "http"
url = "https://api.example.com/v1/time"
method = "GET"               # optional: body, headers = ["Name: value"]

[[endpoints]]
name = "fix"
protocol = "tcp"             # handshake RTT only
host = "fix.example.com"
port = 9000
```

Every endpoint may also set `role` (`order` or `md`; default `md` for streams, `order` otherwise) and `args = [...]`, extra `cex_ws` options for that endpoint. A top-level `[weights]` table (`order = 0.5`, `md = 0.5`) sets the roles' weights in the combined score. `hunt.py run --profile P` probes the TCP handshake to the IPs of all endpoints (each on its own port, labelled by endpoint in the report); `--endpoint NAME` runs that endpoint's own protocol. Before adding an exchange, check that its endpoints are not behind a CDN (see [who answers the SYN](#measurement-modes)) and that the request methods are public.

## What the Placement Group Gives You

A partition placement group keeps up to 7 partitions per AZ on separate sets of racks, so a fleet spread over the partitions samples different racks. It does not guarantee different spines or buildings. To sample more widely:
- add `unplaced_instance_count` instances outside the group;
- launch in batches: increase `instance_count` and apply again later, since each launch draws placements from current capacity.

The group also narrows the capacity pool, so launches fail with insufficient capacity (ICE) more often, especially for metal sizes.

## Outputs

- `instances`: instance index (used by `keep_instances`) to instance ID, partition and whether it is in the placement group
- `instance_public_ip_client_ppg`: public IPs of all instances
- `instance_id_and_partition_id_mapping`: instance-to-partition mapping
- `s3_bucket`, `aws_region`: read by `scripts/hunt.py`

## Troubleshooting

### Cloud-init Issues

```bash
cat /var/log/cloud-init-output.log
```

### Verify CPU Configuration

```bash
# What the boot script applied, skipped or failed (latest boot last)
grep HUNT: /var/log/cloud-init-output.log

# Isolated cores and active boot parameters
cat /sys/devices/system/cpu/isolated /proc/cmdline

# Interrupts per CPU: device IRQs should only count up on cores 0-1
cat /proc/interrupts
```

### Run cex_ws by Hand

```bash
aws ssm start-session --target <instance-id> --region ap-northeast-1
sudo taskset -c "$(cat /sys/devices/system/cpu/isolated)" /share/bench/sources/cex_ws/target/release/cex_ws probe --targets <ip> --count 50
```

`hunt.py` leaves each run's files under `/home/ec2-user/results/<run-id>/` on the instances; `summary.txt` has the exit status.

## Development

```bash
terraform test                      # offline Terraform checks with a mock AWS provider (needs sources/source.tar.gz)
python3 -m unittest tests/test_hunt.py
cd sources/cex_ws && cargo test
```

## Project Structure

```
.
├── main.tf                    # Main Terraform configuration
├── moved.tf                   # State address migrations
├── variables.tf               # Variable definitions
├── providers.tf               # AWS provider configuration
├── terraform.tfvars.example   # Example variable values
├── pack_source.sh             # Packages the Rust source
├── scripts/
│   ├── client.sh              # EC2 boot script: host tuning and client build
│   ├── cloud-config.yaml      # Cloud-init configuration
│   └── hunt.py                # Synchronized runs, result collection and ranking
├── targets/
│   └── binance-usdm.toml      # Target profile: Binance USDⓈ-M futures endpoints
├── tests/
│   ├── hunt.tftest.hcl        # Offline `terraform test` checks (mock AWS)
│   └── test_hunt.py           # Ranking, significance and profile checks for hunt.py
└── sources/
    └── cex_ws/                # Rust measurement client
        └── src/
            ├── main.rs        # CLI: probe, ws, http, stream
            ├── common.rs      # Time grid, connection layout, stats, CSV, TCP_INFO
            ├── probe.rs       # TCP handshake RTT
            ├── ws.rs          # WebSocket round trips
            ├── http.rs        # HTTP round trips
            └── stream.rs      # Stream event arrival
```
