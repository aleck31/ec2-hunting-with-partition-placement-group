#!/bin/bash
# Host tuning and client build; cloud-init runs it on every boot. Rendered by Terraform templatefile(): never write a dollar-brace in shell code.
set -euo pipefail
trap 'echo "HUNT: ERROR line $LINENO: $BASH_COMMAND" >&2' ERR

log() { echo "HUNT: $*"; }

# SMT should already be off via cpu_options; if the instance came up with it, turn it off before counting cores
if [ "$(cat /sys/devices/system/cpu/smt/active 2>/dev/null || echo 0)" = 1 ]; then
  if echo off 2>/dev/null > /sys/devices/system/cpu/smt/control; then log "smt: was on, turned off"; else log "ERROR smt: on and could not be turned off"; fi
else
  log "smt: off"
fi

# Online CPUs; nproc would only count this process's affinity, which excludes isolated cores
NCPU=$(getconf _NPROCESSORS_ONLN)
HOUSEKEEPING=0-1
ISOLATED=2-$((NCPU - 1))
BIN=/share/bench/sources/cex_ws/target/release/cex_ws
REBOOT=0

# Turbo and governor are controllable only where the platform exposes them (bare metal)
if echo 1 2>/dev/null > /sys/devices/system/cpu/intel_pstate/no_turbo; then
  log "turbo: disabled"
else
  log "turbo: not exposed, frequency managed by the platform"
fi
if [ -e /sys/devices/system/cpu/cpu0/cpufreq/scaling_governor ]; then
  ok=1
  for g in /sys/devices/system/cpu/cpu*/cpufreq/scaling_governor; do echo performance 2>/dev/null > "$g" || ok=0; done
  if [ "$ok" -eq 1 ]; then log "governor: performance"; else log "governor: could not set performance"; fi
else
  log "governor: cpufreq not exposed, frequency managed by the platform"
fi

cat > /etc/sysctl.d/90-hunt.conf <<'EOF'
net.core.busy_poll = 50
net.core.busy_read = 50
kernel.numa_balancing = 0
kernel.timer_migration = 0
EOF
sysctl -q -e -p /etc/sysctl.d/90-hunt.conf

# Adaptive RX moderation varies the receive delay per host; turn moderation off (fine at this request rate)
IFACE=$(ip -o route show default 2>/dev/null | awk '{print $5; exit}') || true
if [ -n "$IFACE" ] && ethtool -C "$IFACE" adaptive-rx off rx-usecs 0 2>/dev/null; then
  log "nic: $IFACE interrupt moderation off"
else
  log "nic: interrupt moderation unchanged on [$IFACE]"
fi

if [ "$NCPU" -ge 3 ]; then
  if echo 3 2>/dev/null > /sys/devices/virtual/workqueue/cpumask; then
    log "workqueue: unbound work on $HOUSEKEEPING"
  else
    log "workqueue: cpumask unchanged"
  fi

  # Keep all device interrupts on the housekeeping cores; irqbalance would spread them back
  if systemctl is-enabled --quiet irqbalance 2>/dev/null; then systemctl disable --now irqbalance; fi
  pinned=0
  for f in /proc/irq/[0-9]*/smp_affinity_list; do
    if echo "$HOUSEKEEPING" 2>/dev/null > "$f"; then pinned=$((pinned + 1)); fi
  done
  log "irq: $pinned IRQs pinned to $HOUSEKEEPING"
fi

# Toolchain once, rebuild whenever the source in S3 changed (update a running instance by rebooting it)
export RUSTUP_HOME=/share/.rustup CARGO_HOME=/share/.cargo
if [ ! -x /share/.cargo/bin/cargo ]; then
  mkdir -p /share
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /share/rust-init.sh
  sh /share/rust-init.sh -y --profile minimal --default-toolchain stable
fi
aws s3 cp --only-show-errors "s3://${s3_bucket}/source.tar.gz" /share/source.tar.gz
SRC_MD5=$(md5sum /share/source.tar.gz | cut -d' ' -f1)
if [ ! -x "$BIN" ] || [ "$SRC_MD5" != "$(cat /share/source.md5 2>/dev/null || true)" ]; then
  mkdir -p /share/bench
  tar -zxf /share/source.tar.gz -C /share/bench
  (cd /share/bench/sources/cex_ws && /share/.cargo/bin/cargo build --release)
  echo "$SRC_MD5" > /share/source.md5
  log "client: built $BIN"
else
  log "client: source unchanged"
fi

# CPU isolation needs kernel boot parameters, so the first boot sets them and reboots once.
# idle=poll keeps idle cores spinning instead of halting, so waking up for a response is fast.
CMDLINE="isolcpus=$ISOLATED nohz_full=$ISOLATED rcu_nocbs=$ISOLATED irqaffinity=$HOUSEKEEPING nosmt idle=poll nosoftlockup skew_tick=1 nmi_watchdog=0 audit=0 transparent_hugepage=never clocksource=tsc tsc=reliable"
MARKER=/var/lib/hunt/cmdline-set
if [ "$NCPU" -lt 3 ]; then
  log "cmdline: only $NCPU CPUs, CPU isolation skipped"
elif grep -qw "isolcpus=$ISOLATED" /proc/cmdline; then
  log "cmdline: active"
elif grep -q 'isolcpus=' /proc/cmdline; then
  log "cmdline: the AMI sets its own isolcpus, left unchanged"
elif [ -e "$MARKER" ]; then
  log "ERROR cmdline: set but not active after reboot, check 'grubby --info=DEFAULT'"
elif command -v grubby >/dev/null || dnf -y -q install grubby; then
  grubby --update-kernel=ALL --args="$CMDLINE"
  mkdir -p /var/lib/hunt && touch "$MARKER"
  log "cmdline: set, rebooting to apply"
  REBOOT=1
else
  log "ERROR cmdline: grubby not found, CPU isolation skipped"
fi

log "status: isolated=[$(cat /sys/devices/system/cpu/isolated)] nohz_full=[$(cat /sys/devices/system/cpu/nohz_full 2>/dev/null || echo n/a)] clocksource=$(cat /sys/devices/system/clocksource/clocksource0/current_clocksource)"
if [ "$REBOOT" -eq 1 ]; then systemctl --no-block reboot; fi

### To start the benchmark on boot (after the tuning reboot), uncomment and set the target
#TARGET_IP=203.0.113.10
#WS_URL=wss://ws-fapi.example-exchange.com/ws-fapi/v1
#if [ "$REBOOT" -eq 0 ]; then nohup taskset -c "$(cat /sys/devices/system/cpu/isolated)" "$BIN" "$TARGET_IP" --url "$WS_URL" > "/home/ec2-user/$(ec2-metadata -i | awk '{print $2}').log" 2>&1 & fi
