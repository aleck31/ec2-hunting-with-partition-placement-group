# Measurement

How `cex_ws` and `scripts/hunt.py` measure, how to read the report, and where the numbers stop.

Scope: this tool picks the host. Which market-data IPs the chosen host should subscribe to, and what subscribing over several connections at once gains, can only be measured on that host and belong to its trading application, not to hunting.

## Modes

All modes take `--start-at <unix-ms>` (a time grid shared by all instances; `hunt.py` sets it) and `--out <csv>` (every sample). Sends are scheduled on the grid and spun onto it, so instances hit the exchange together, within their clock-sync error; each sample records its offset from the grid. Options and defaults: `cex_ws <mode> --help`.

| Mode | Measures |
|---|---|
| `probe` | TCP handshake RTT, the ranking metric: the kernel's RTT sample from SYN/SYN-ACK (`TCP_INFO`), free of user-space wakeup delay. One new connection per slot, targets (`IP` or `IP:port`) round-robin, first handshakes per target discarded. Works for any TCP service (HTTPS, WebSocket, FIX) |
| `ws` | Round trip on established WebSocket connections, from the request write to the response frame, before JSON parsing. `--request` is a JSON template whose `{id}` comes back in `--id-field` |
| `http` | Round trip on keep-alive HTTP connections, from the request write to the parsed response head |
| `stream` | Wall-clock arrival of each stream event, with the exchange's event time; reported, not ranked (see the clock rule) |

`ws`, `http` and `stream` open TCP straight to the target IP, with TLS SNI and `Host` from the URL, over several sequential `--connections`.

## What the numbers mean

- **Who answers the SYN.** Probe RTT ends wherever TCP terminates: at the exchange's own front ends or a pass-through load balancer, that is the path to the exchange; behind a CDN or an ALB (Binance `fapi.binance.com` is a CNAME to one), it only reaches that edge.
- **Round trips include exchange processing.** On Binance ws-fapi from `c8i.2xlarge` in apne1-az4, the handshake took ~0.2 ms and a `depth` round trip ~0.9 ms; placements differed by 10–30 µs, below the exchange's own jitter, which is why probes rank and round trips only confirm. The kernel's srtt tracks the round trip, since the exchange's ACK rides on its response.
- **Backends.** One IP can front several gateways and each connection lands on one; the report's per-connection p50s show when results depend on the backend rather than the placement.
- **Clock rule: rank only on same-machine time differences.** Instances sync to the basic Amazon Time Sync Service (`169.254.169.123`; chrony error bounds of 70–90 µs measured, logged at every boot). That aligns sending slots, but not arrival times compared across machines, where placements differ by less than the clock error; so stream runs are not ranked, and distance to market data comes from probes to its IPs (role `md`). PHC and NIC hardware timestamps would remove this error but need a precision-time placement group, which a partition placement group cannot use.
- **Not measured: the order path.** Order round trips need signed requests and real orders; testnets run on other infrastructure.

## Reading the report

- **median Δ vs fleet**: per slot, each instance's value minus the fleet median at that moment, then the median per instance with a bootstrap 95% CI. Pairing by time cancels exchange-side load swings, which are often larger than placement differences. A trimmed mean breaks ties.
- **verdict**: winner versus runner-up on their paired differences. A lead counts if its CI excludes zero and it is at least `--min-diff-us` (5 µs); an unresolved lead comes with the sample count that would resolve it.
- **per target**: the best instance for each IP. A hostname's IPs can rank instances differently, so pin production connections to the IP you hunted for.
- **per role**: separate rankings for order entry (`order`) and market data (`md`), their rank correlation, a score weighted by the profile's `[weights]` (or `compare --weights order=0.7,md=0.3`), and the suggested `keep_instances`.
- `results/<run-id>/combined.csv` has every sample for further analysis.

A winner is credible when it is significant in each run and stays first across runs at different times of day.

## How many samples

The CI half-width shrinks with the square root of the sample count. In the Binance run (paired differences with a standard deviation of ~140 µs) it was ±19 µs at 100 paired samples, ±11 µs at 300, ±6 µs at 1,000 and ±3.5 µs at 3,000. Resolving a 10 µs lead needs about ±5 µs: some 1,500 paired samples for the ranking, counted over all IPs together (e.g. 30 IPs × 50). Choosing the IP to pin needs ~300 per IP for ±10 µs. Repeating at other times says more than one longer run.

## Targets and limits

`hunt.py` resolves hostnames on an instance, through the VPC resolver and the zone's authoritative name servers: answers depend on where the query comes from (outside AWS, `fapi.binance.com` may resolve to CloudFront), rotate, and are cached by the VPC resolver for the TTL. Hosts with many IPs (Binance `fstream`: ~470) are sampled to `--max-targets-per-host` (16), and probe runs estimated to exceed `--max-minutes` (10) are refused.

Use documented endpoints and ports only, within the exchange's published limits per source IP. Binance USDⓈ-M, for example: REST and WebSocket API share 2,400 request weight per minute per IP, a WebSocket handshake costs 5 and a `depth` request 2, so the default `ws` run uses about half; stream connections accept at most 10 incoming messages per second, and IPs repeatedly disconnected may be banned. No exchange publishes limits for bare TCP handshakes, so keep probes gentle: the defaults open 40 connections per second per instance.

## Placement group

A partition placement group keeps up to 7 partitions per AZ on separate sets of racks, so the fleet samples different racks, but it doesn't guarantee different spines or buildings. To sample more widely, add `unplaced_instance_count` instances outside the group, or launch in batches (raise `instance_count` and apply again later). The group also narrows the capacity pool, so launches hit insufficient capacity (ICE) more often, especially for metal sizes.

## Host tuning

`scripts/client.sh` runs at every boot and logs each step as `HUNT: ...` in `/var/log/cloud-init-output.log`:
- SMT off (`cpu_options` with the type's core count and one thread per core, plus `nosmt`)
- cores 2..N-1 isolated (`isolcpus`, `nohz_full`, `rcu_nocbs`), set on first boot with one reboot; the benchmark is pinned to them
- device interrupts, unbound workqueues and system services on cores 0-1, irqbalance off
- `idle=poll`, `clocksource=tsc`, NMI watchdog, audit and THP off, socket busy polling, NIC interrupt moderation off
- Turbo and governor set only where exposed (bare metal); the clock source and its error terms logged
- the client rebuilt when the source in S3 changed
