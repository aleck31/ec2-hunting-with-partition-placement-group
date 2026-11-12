#!/usr/bin/env python3
"""Run one synchronized measurement on every hunted instance, collect the samples from S3 and rank the instances.

  scripts/hunt.py run --profile targets/binance-usdm.toml                    # TCP handshake probe of every endpoint
  scripts/hunt.py run --profile targets/binance-usdm.toml --endpoint ws-api  # that endpoint's own protocol
  scripts/hunt.py run probe --hosts ws-fapi.binance.com -- --count 500       # ad hoc, without a profile
  scripts/hunt.py run ws -- 16.76.26.173 --url wss://ws-fapi.binance.com/ws-fapi/v1 --connections 5
  scripts/hunt.py compare results/<run-id> [results/<run-id> ...]

A profile (targets/*.toml) describes an exchange's endpoints and how to measure each; see README. Arguments after
"--" go to cex_ws unchanged. Run from the repo root (it reads `terraform output`); needs the AWS CLI with credentials
for the account and Python 3.11+. Ranking compares instances slot by slot (probe, ws) or event by
event (stream) against the fleet median, so load swings on the exchange side cancel out.
"""

import argparse
import csv
import ipaddress
import json
import random
import re
import shlex
import statistics
import subprocess
import sys
import time
import tomllib
from pathlib import Path
from urllib.parse import urlsplit

BIN = "/share/bench/sources/cex_ws/target/release/cex_ws"
BARS = " ▁▂▃▄▅▆▇█"
# Profile protocol -> cex_ws mode
MODES = {"tcp": "probe", "ws": "ws", "stream": "stream", "http": "http"}
DEFAULT_PORTS = {"https": 443, "wss": 443, "http": 80, "ws": 80}


def sh(*args: str) -> str:
    return subprocess.run(args, check=True, capture_output=True, text=True).stdout


def aws(region: str, *args: str) -> dict:
    out = sh("aws", "--region", region, "--output", "json", *args)
    return json.loads(out) if out.strip() else {}


def ssm_run(region: str, ids: list, commands: list, timeout_s: int) -> dict:
    """Runs a shell script on the instances via SSM and returns {instance_id: (status, stdout)}."""
    params = json.dumps({"commands": commands, "executionTimeout": [str(timeout_s)]})
    cid = aws(region, "ssm", "send-command", "--document-name", "AWS-RunShellScript", "--timeout-seconds", "600",
              "--parameters", params, "--instance-ids", *ids)["Command"]["CommandId"]
    deadline = time.time() + timeout_s + 300
    while time.time() < deadline:
        time.sleep(10)
        inv = aws(region, "ssm", "list-command-invocations", "--command-id", cid)["CommandInvocations"]
        running = [i for i in inv if i["Status"] in ("Pending", "InProgress", "Delayed")]
        if len(inv) == len(ids) and not running:
            break
        print(f"  {len(ids) - len(running)}/{len(ids)} instances done", file=sys.stderr)
    results = {}
    for iid in ids:
        r = aws(region, "ssm", "get-command-invocation", "--command-id", cid, "--instance-id", iid)
        results[iid] = (r["Status"], r.get("StandardOutputContent", "") + r.get("StandardErrorContent", ""))
    return results


def resolve(region: str, iid: str, hosts: list, rounds: int) -> dict:
    """{host: [IPv4, ...]} from an instance. Each answer is a rotating subset, and the VPC resolver caches it for the
    TTL (600 s for Binance), so this asks the zone's authoritative name servers directly, plus the local resolver."""
    names = " ".join(shlex.quote(h) for h in hosts)
    dig = "dig +time=2 +tries=1"
    script = "\n".join([
        "command -v dig >/dev/null || dnf install -y -q bind-utils >/dev/null 2>&1 || true",
        f"for h in {names}; do",
        "  getent ahostsv4 \"$h\" | awk -v h=\"$h\" '{print h, $1}'",
        # Follow a CNAME (e.g. to a load balancer name), then walk up to the zone apex that has NS records
        f"  t=$({dig} +short CNAME \"$h\" | tail -1); t=${{t%.}}; t=${{t:-$h}}",
        f"  z=$t; ns=''; while [ -z \"$ns\" ] && [ \"${{z#*.}}\" != \"$z\" ]; do ns=$({dig} +noall +answer NS \"$z\" | awk '$4==\"NS\"{{print $5}}'); [ -n \"$ns\" ] || z=${{z#*.}}; done",
        f"  for n in $ns; do for i in $(seq {rounds}); do {dig} +short A \"$t\" @\"$n\" | awk -v h=\"$h\" '/^[0-9.]+$/{{print h, $1}}'; done; done",
        "done | sort -u",
    ])
    status, out = ssm_run(region, [iid], [script], rounds * 30 + 120)[iid]
    if status != "Success":
        sys.exit(f"Resolving {', '.join(hosts)} on {iid} ended with {status}:\n{out}")
    by_host = {}
    for line in out.splitlines():
        parts = line.split()
        if len(parts) == 2 and parts[0] in hosts and re.fullmatch(r"\d+\.\d+\.\d+\.\d+", parts[1]):
            by_host.setdefault(parts[0], set()).add(parts[1])
    return {h: sorted(ips, key=ipaddress.IPv4Address) for h, ips in by_host.items()}


def instance_script(run_id: str, bucket: str, region: str, mode: str, cex_args: list, start_at: int) -> list:
    args = " ".join(shlex.quote(a) for a in [mode, *cex_args, "--start-at", str(start_at), "--out", "samples.csv"])
    return [
        "set -u",
        f"OUT=/home/ec2-user/results/{run_id}; mkdir -p $OUT; cd $OUT",
        "T=$(curl -sf -X PUT http://169.254.169.254/latest/api/token -H 'X-aws-ec2-metadata-token-ttl-seconds: 600')",
        'md() { curl -sf -H "X-aws-ec2-metadata-token: $T" http://169.254.169.254/latest/meta-data/$1 || true; }',
        "IID=$(md instance-id)",
        """printf '{"instance_id":"%s","instance_type":"%s","az_id":"%s","partition":"%s"}\\n' "$IID" "$(md instance-type)" "$(md placement/availability-zone-id)" "$(md placement/partition-number)" > meta.json""",
        "chronyc tracking > chrony-before.txt 2>&1 || true",
        'CPUS=$(cat /sys/devices/system/cpu/isolated); [ -n "$CPUS" ] || { CPUS=0-$(($(getconf _NPROCESSORS_ONLN) - 1)); echo "WARNING: no isolated CPUs" >> summary.txt; }',
        f'taskset -c "$CPUS" {BIN} {args} >> summary.txt 2>&1; echo "exit=$?" >> summary.txt',
        "chronyc tracking > chrony-after.txt 2>&1 || true",
        f"aws --region {region} s3 cp --only-show-errors --recursive . s3://{bucket}/results/{run_id}/$IID/",
        "tail -n 60 summary.txt",
    ]


def load_profile(path: str) -> dict:
    with open(path, "rb") as f:
        profile = tomllib.load(f)
    for ep in profile.get("endpoints", []):
        if ep.get("protocol") not in MODES:
            sys.exit(f"{path}: endpoint {ep.get('name')!r} needs protocol = one of {', '.join(MODES)}")
        if ep["protocol"] != "tcp" and "url" not in ep:
            sys.exit(f"{path}: endpoint {ep['name']!r} ({ep['protocol']}) needs a url")
    if not profile.get("endpoints"):
        sys.exit(f"{path}: no [[endpoints]]")
    return profile


def host_port(ep: dict) -> tuple:
    """Where TCP goes for an endpoint: from its url, or host and port for plain tcp endpoints."""
    if "url" in ep:
        u = urlsplit(ep["url"])
        return u.hostname, ep.get("port") or u.port or DEFAULT_PORTS[u.scheme]
    return ep["host"], ep.get("port", 443)


def endpoint_args(ep: dict) -> list:
    """cex_ws options for an endpoint, after the target IP."""
    args = ["--url", ep["url"]] if "url" in ep else []
    for key, flag in (("request", "--request"), ("id_field", "--id-field"), ("subscribe", "--subscribe"),
                      ("key_field", "--key"), ("event_time_field", "--event-time"), ("method", "--method"),
                      ("body", "--body")):
        if key in ep:
            args += [flag, ep[key]]
    for header in ep.get("headers", []):
        args += ["--header", header]
    return args + list(ep.get("args", []))


def probe_targets(endpoints: list, resolved: dict) -> tuple:
    """Every IP:port behind the endpoints, and a label per target naming its endpoints."""
    labels = {}
    for ep in endpoints:
        host, port = host_port(ep)
        for ip in resolved.get(host, []):
            labels.setdefault(f"{ip}:{port}", []).append(ep["name"])
    targets = sorted(labels, key=target_key)
    return targets, {t: "+".join(labels[t]) for t in targets}


def sample_hosts(resolved: dict, limit: int) -> dict:
    """At most `limit` IPs per host. Some hosts front hundreds of rotating IPs (Binance fstream: ~470), where the useful
    question is how an instance does across them, so a fixed-seed sample keeps probe runs bounded and comparable."""
    out = {}
    for host, ips in resolved.items():
        pick = sorted(random.Random(host).sample(ips, limit), key=ipaddress.IPv4Address) if len(ips) > limit else ips
        note = f" (sampled {len(pick)} of {len(ips)})" if len(pick) < len(ips) else ""
        print(f"Resolved {len(ips)} IPs for {host}{note}: {' '.join(pick)}", flush=True)
        out[host] = pick
    return out


def probe_minutes(n_targets: int, cex_args: list) -> float:
    """Run time implied by the probe options (cex_ws defaults where not given)."""
    opt = {"--count": 200, "--warmup": 2, "--interval-ms": 25}
    for i, arg in enumerate(cex_args[:-1]):
        if arg in opt:
            opt[arg] = int(cex_args[i + 1])
    return n_targets * (opt["--count"] + opt["--warmup"]) * opt["--interval-ms"] / 60000


def target_key(t: str) -> tuple:
    ip, _, port = t.partition(":")
    return ipaddress.IPv4Address(ip), int(port or 0)


def cmd_run(a) -> None:
    outs = {k: v["value"] for k, v in json.loads(sh("terraform", f"-chdir={a.tf_dir}", "output", "-json")).items()}
    region, bucket, instances = outs["aws_region"], outs["s3_bucket"], outs["instances"]
    ids = [instances[k]["id"] for k in sorted(instances, key=int)]
    cex_args, hosts, labels, mode = list(a.cex_args), {}, {}, a.mode
    profile = load_profile(a.profile) if a.profile else None
    endpoints = profile["endpoints"] if profile else []
    if a.endpoint:
        endpoints = [ep for ep in endpoints if ep["name"] == a.endpoint]
        if not endpoints:
            sys.exit(f"No endpoint {a.endpoint!r} in {a.profile}")
        mode = mode or MODES[endpoints[0]["protocol"]]
    elif profile:
        mode = mode or "probe"
    if not mode:
        sys.exit("Give a mode, or --profile")

    if mode == "probe" and (endpoints or a.hosts):
        names = sorted({host_port(ep)[0] for ep in endpoints} | set(a.hosts.split(",") if a.hosts else []))
        hosts = sample_hosts(resolve(region, ids[0], names, a.resolve_rounds), a.max_targets_per_host)
        adhoc = [{"name": h, "protocol": "tcp", "host": h, "port": 443} for h in (a.hosts.split(",") if a.hosts else [])]
        targets, labels = probe_targets(endpoints + adhoc, hosts)
        if not targets:
            sys.exit("No IPv4 addresses resolved")
        cex_args += ["--targets", ",".join(targets)]
        minutes = probe_minutes(len(targets), cex_args)
        print(f"Probe: {len(targets)} targets, ~{minutes:.1f} min", flush=True)
        if minutes > a.max_minutes:
            sys.exit(f"Estimated {minutes:.1f} min exceeds --max-minutes {a.max_minutes:g}; lower --count, raise --interval-ms "
                     f"or --max-targets-per-host, or raise --max-minutes")
    elif endpoints:
        ep = endpoints[0]
        ip = a.ip
        if not ip:
            host = host_port(ep)[0]
            ips = resolve(region, ids[0], [host], 1).get(host, [])
            if not ips:
                sys.exit(f"No IPv4 address resolved for {host}")
            ip = ips[0]
            print(f"Using {ip} for {host} (of {len(ips)}); pass --ip to pick the one you hunted for", flush=True)
        cex_args = [ip, *endpoint_args(ep), *cex_args]
    run_id = time.strftime("%Y%m%dT%H%M%SZ", time.gmtime()) + f"-{a.endpoint or mode}"
    start_at = int(time.time() * 1000) + a.lead_s * 1000
    print(f"Run {run_id}: {len(ids)} instances, start at {start_at} (in {a.lead_s} s)", flush=True)
    results = ssm_run(region, ids, instance_script(run_id, bucket, region, mode, cex_args, start_at), a.timeout_s)
    for iid, (status, out) in results.items():
        print(f"===== {iid} {status}\n{out.strip()}")
    local = Path(a.results_dir) / run_id
    sh("aws", "--region", region, "s3", "cp", "--only-show-errors", "--recursive", f"s3://{bucket}/results/{run_id}/", str(local))
    meta = {"mode": mode, "profile": profile and profile.get("name"), "endpoint": a.endpoint, "start_at": start_at,
            "cex_args": cex_args, "hosts": hosts, "target_labels": labels, "instances": instances}
    (local / "run.json").write_text(json.dumps(meta, indent=2))
    print(f"\nResults in {local}\n")
    compare([local])


# ---------- analysis ----------

def clock_error_us(inst_dir: Path):
    """chrony's bound on this clock's error, |offset| + root delay / 2 + root dispersion, worst of before/after."""
    worst = None
    for name in ("chrony-before.txt", "chrony-after.txt"):
        p = inst_dir / name
        text = p.read_text() if p.exists() else ""
        f = {k: float(v) for k, v in re.findall(r"^(System time|Root delay|Root dispersion)\s*:\s*([\d.]+)", text, re.M)}
        if len(f) == 3:
            bound = (f["System time"] + f["Root delay"] / 2 + f["Root dispersion"]) * 1e6
            worst = bound if worst is None else max(worst, bound)
    return worst


def load_run(run_dir: Path) -> dict:
    run = json.loads((run_dir / "run.json").read_text()) if (run_dir / "run.json").exists() else {}
    index = {v["id"]: k for k, v in run.get("instances", {}).items()}
    insts = []
    for d in sorted(p for p in run_dir.iterdir() if p.is_dir()):
        if not (d / "samples.csv").exists():
            continue
        meta = json.loads((d / "meta.json").read_text()) if (d / "meta.json").exists() else {"instance_id": d.name}
        with open(d / "samples.csv", newline="") as f:
            rows = list(csv.DictReader(f))
        iid = meta["instance_id"]
        insts.append({"name": f"{index.get(iid, '?')}:{iid}", "id": iid, "partition": meta.get("partition") or "-",
                      "rows": rows, "clock_us": clock_error_us(d)})
    header = set(insts[0]["rows"][0]) if insts and insts[0]["rows"] else set()
    mode = ("probe" if "syn_rtt_us" in header else "http" if "http_status" in header else "ws" if "rtt_us" in header
            else "stream" if "recv_wall_us" in header else run.get("mode"))
    labels = run.get("target_labels") or {ip: h for h, ips in run.get("hosts", {}).items() for ip in ips}
    return {"dir": run_dir, "mode": mode, "insts": insts, "host_of": labels}


def series(inst: dict, mode: str) -> dict:
    """Per instance {key: value µs}: slot -> RTT for probe/ws, event key -> first arrival for stream."""
    out = {}
    for r in inst["rows"]:
        if mode == "stream":
            k, v = r["key"], float(r["recv_wall_us"])
            out[k] = min(v, out.get(k, v))
        elif r["status"] == "ok" and r["warmup"] == "0":
            # Kernel SYN RTT, falling back to connect() time where the kernel gave no sample
            v = (float(r["syn_rtt_us"]) or float(r["connect_us"])) if mode == "probe" else float(r["rtt_us"])
            out[int(r["slot"])] = v
    return out


def relative(all_series: dict) -> dict:
    """Each instance's value minus the fleet median for the same slot or event."""
    rel = {name: [] for name in all_series}
    for k in set().union(*all_series.values()):
        vals = {n: s[k] for n, s in all_series.items() if k in s}
        if len(vals) < 2:
            continue
        ref = statistics.median(vals.values())
        for n, v in vals.items():
            rel[n].append(v - ref)
    return rel


def median_ci(values: list, b: int = 300, seed: int = 1) -> tuple:
    """Bootstrap 95% confidence interval of the median."""
    if not values:
        return float("nan"), float("nan")
    rng = random.Random(seed)
    meds = sorted(statistics.median(rng.choices(values, k=len(values))) for _ in range(b))
    return meds[int(0.025 * b)], meds[int(0.975 * b) - 1]


def percentiles(values: list) -> str:
    v = sorted(values)
    n = len(v)
    if not n:
        return "n=0"
    cols = [f"min={v[0]:.0f}"]
    for label, p in (("p50", 0.5), ("p90", 0.9), ("p99", 0.99), ("p99.9", 0.999)):
        cols.append(f"{label}={v[round((n - 1) * p)]:.0f}" if n * (1 - p) >= 5 else f"{label}=-")
    return " ".join(cols + [f"max={v[-1]:.0f}", f"n={n}"])


def histograms(named: dict, bins: int = 24) -> list:
    """One sparkline per instance over shared bins between the pooled p0.5 and p99.5."""
    pooled = sorted(v for vals in named.values() for v in vals)
    if len(pooled) < 2:
        return []
    lo, hi = pooled[int(0.005 * len(pooled))], pooled[int(0.995 * len(pooled)) - 1]
    width = (hi - lo) / bins or 1
    lines = [f"  histogram {lo:.0f}..{hi:.0f} µs, {width:.1f} µs per bin (outliers in the end bins)"]
    for name, vals in named.items():
        counts = [0] * bins
        for v in vals:
            counts[min(bins - 1, max(0, int((v - lo) / width)))] += 1
        top = max(counts) or 1
        lines.append(f"  {name:<24} |{''.join(BARS[round(c / top * (len(BARS) - 1))] for c in counts)}|")
    return lines


def trimmed_mean(values: list, cut: float = 0.1) -> float:
    v = sorted(values)
    k = int(len(v) * cut)
    v = v[k:len(v) - k] or v
    return sum(v) / len(v) if v else float("nan")


def rank(run: dict) -> list:
    """Instances ordered best first by median relative value; the 10% trimmed mean breaks ties (values are whole µs)."""
    mode = run["mode"]
    s = {i["name"]: series(i, mode) for i in run["insts"]}
    rel = relative(s)
    rows = []
    for inst in run["insts"]:
        lo, hi = median_ci(rel[inst["name"]])
        med = statistics.median(rel[inst["name"]]) if rel[inst["name"]] else float("nan")
        rows.append({"inst": inst, "median": med, "mean": trimmed_mean(rel[inst["name"]]), "ci": (lo, hi),
                     "rel": rel[inst["name"]], "values": list(s[inst["name"]].values())})
    return sorted(rows, key=lambda r: (r["median"], r["mean"]))


def verdict(run: dict, ranked: list, min_diff_us: float = 5.0) -> str:
    if len(ranked) < 2:
        return "only one instance"
    a, b = ranked[0], ranked[1]
    sa, sb = series(a["inst"], run["mode"]), series(b["inst"], run["mode"])
    diffs = [sa[k] - sb[k] for k in sa.keys() & sb.keys()]
    if not diffs:
        return "no common samples"
    med = statistics.median(diffs)
    lo, hi = median_ci(diffs)
    text = f"{a['inst']['name']} vs runner-up {b['inst']['name']}: median paired diff {med:+.1f} µs, 95% CI [{lo:+.1f}, {hi:+.1f}]"
    significant = hi < 0
    if run["mode"] == "stream":
        ca, cb = a["inst"]["clock_us"], b["inst"]["clock_us"]
        bound = (ca or 0) + (cb or 0)
        text += f", clock error bound {bound:.0f} µs" + ("" if ca is not None and cb is not None else " (chrony data missing)")
        significant = significant and abs(med) > bound
    if significant and abs(med) < min_diff_us:
        return text + f" -> negligible (below {min_diff_us:g} µs)"
    return text + (" -> significant" if significant else " -> NOT significant")


def report(run: dict, min_diff_us: float = 5.0) -> list:
    mode, ranked = run["mode"], rank(run)
    metric = {"probe": "TCP handshake RTT (kernel)", "ws": "app round trip", "http": "HTTP round trip", "stream": "arrival time"}[mode]
    print(f"Run {run['dir'].name}: {mode}, {len(ranked)} instances, metric: {metric}")
    print(f"  {'rank':<5}{'instance':<24}{'part':>5}  {'median Δ vs fleet':>18}  {'95% CI':>18}  {'trim. mean Δ':>12}  distribution (µs)")
    for i, r in enumerate(ranked, 1):
        # Stream arrivals are wall-clock times, so only their offsets from the fleet are meaningful
        dist = percentiles(r["rel" if mode == "stream" else "values"])
        clock = f" clock±{r['inst']['clock_us']:.0f}" if mode == "stream" and r["inst"]["clock_us"] is not None else ""
        print(f"  {i:<5}{r['inst']['name']:<24}{r['inst']['partition']:>5}  {r['median']:>+15.1f} µs  [{r['ci'][0]:+7.1f}, {r['ci'][1]:+7.1f}]  {r['mean']:>+9.1f} µs  {dist}{clock}")
    print("\n".join(histograms({r["inst"]["name"]: r["rel" if mode == "stream" else "values"] for r in ranked})))
    if mode == "probe":
        per_target(run)
    if mode in ("ws", "http"):
        for r in ranked:
            by_conn = {}
            for row in r["inst"]["rows"]:
                if row["status"] == "ok" and row["warmup"] == "0":
                    by_conn.setdefault(row["conn"], []).append(float(row["rtt_us"]))
            print(f"  {r['inst']['name']:<24} p50 per connection: " + " ".join(f"{statistics.median(v):.0f}" for _, v in sorted(by_conn.items())))
    print("  " + verdict(run, ranked, min_diff_us) + "\n")
    export(run)
    return [r["inst"]["id"] for r in ranked]


def per_target(run: dict) -> None:
    """Best instance per target IP by p50; rankings can differ between the IPs behind one hostname."""
    p50 = {}
    for inst in run["insts"]:
        by_target = {}
        for r in inst["rows"]:
            if r["status"] == "ok" and r["warmup"] == "0":
                by_target.setdefault(r["target"], []).append(float(r["syn_rtt_us"]) or float(r["connect_us"]))
        for t, vals in by_target.items():
            p50.setdefault(t, {})[inst["name"]] = statistics.median(vals)
    wins = {}
    print("  per target (p50 µs): target, endpoint, best instance, best, runner-up")
    for t in sorted(p50, key=target_key):
        order = sorted(p50[t].items(), key=lambda kv: kv[1])
        wins[order[0][0]] = wins.get(order[0][0], 0) + 1
        second = f"{order[1][1]:.0f}" if len(order) > 1 else "-"
        label = run["host_of"].get(t) or run["host_of"].get(t.partition(":")[0], "-")
        print(f"    {t:<22} {label:<24} {order[0][0]:<24} {order[0][1]:>6.0f} {second:>6}")
    print("  targets won: " + ", ".join(f"{n}={c}" for n, c in sorted(wins.items(), key=lambda kv: -kv[1])))


def export(run: dict) -> None:
    """All instances' samples in one CSV, for histograms or other offline analysis."""
    insts = run["insts"]
    if not insts or not insts[0]["rows"]:
        return
    cols = list(insts[0]["rows"][0])
    with open(run["dir"] / "combined.csv", "w", newline="") as f:
        w = csv.writer(f)
        w.writerow(["instance", "partition", *cols])
        for inst in insts:
            for r in inst["rows"]:
                w.writerow([inst["name"], inst["partition"], *(r[c] for c in cols)])


def compare(dirs: list, min_diff_us: float = 5.0) -> None:
    rankings = [report(load_run(Path(d)), min_diff_us) for d in dirs]
    if len(rankings) > 1:
        print("Rank per run (a winner should hold across runs taken at different times):")
        for iid in rankings[0]:
            print(f"  {iid:<22} " + " ".join(f"{r.index(iid) + 1 if iid in r else '-':>3}" for r in rankings))
        tops = {r[0] for r in rankings}
        print("  consistent winner: " + (tops.pop() if len(tops) == 1 else "none"))


def main() -> None:
    p = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = p.add_subparsers(dest="cmd", required=True)
    r = sub.add_parser("run", help="run cex_ws on all instances at the same start time and collect results")
    r.add_argument("mode", nargs="?", choices=["probe", "ws", "http", "stream"], help="default: from --profile/--endpoint")
    r.add_argument("--profile", help="targets/*.toml describing the exchange's endpoints")
    r.add_argument("--endpoint", help="measure this profile endpoint with its own protocol (default: probe all)")
    r.add_argument("--ip", help="target IP for ws/http/stream endpoints (default: first resolved)")
    r.add_argument("--hosts", help="probe: extra comma-separated hostnames (port 443) to resolve into --targets")
    r.add_argument("--resolve-rounds", type=int, default=20, help="DNS lookups per host and name server (answers rotate)")
    r.add_argument("--max-targets-per-host", type=int, default=16, help="probe: fixed-seed sample of each host's IPs")
    r.add_argument("--max-minutes", type=float, default=10, help="probe: refuse runs estimated to take longer")
    r.add_argument("--lead-s", type=int, default=20, help="seconds between now and the shared start time")
    r.add_argument("--timeout-s", type=int, default=3600, help="max run time on the instances")
    r.add_argument("--tf-dir", default=".", help="Terraform root to read outputs from")
    r.add_argument("--results-dir", default="results")
    c = sub.add_parser("compare", help="rank instances from downloaded results; several runs show consistency")
    c.add_argument("dirs", nargs="+")
    c.add_argument("--min-diff-us", type=float, default=5.0, help="smaller winner leads are reported as negligible")
    # Everything after the first "--" is passed to cex_ws untouched
    argv = sys.argv[1:]
    cut = argv.index("--") if "--" in argv else len(argv)
    a = p.parse_args(argv[:cut])
    a.cex_args = argv[cut + 1:]
    if a.cmd == "run":
        cmd_run(a)
    else:
        compare(a.dirs, a.min_diff_us)


if __name__ == "__main__":
    main()
