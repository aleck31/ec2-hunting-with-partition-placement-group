# CEX WebSocket EC2 Hunting

EC2 Hunting for CEX WebSocket API latency using AWS EC2 instances with partition placement groups.

## Overview

This project deploys 7 x `c6in.2xlarge` to measure WebSocket latency to CEX API endpoints. The Rust client connects to specific CEX IPs and measures round-trip latency for market data requests using nanosecond-accurate monotonic timestamping.

### The Hunting Path
![](assets/hunting-path.png)

### Architecture

- **EC2 Instances**: 7x c6in.2xlarge instances in partition placement group
- **Placement Strategy**: Partition placement group for network isolation
- **AMI**: your production AMI via `ami_id`, otherwise Amazon Linux 2023 (kernel 6.18)
- **Region**: ap-northeast-1 (Tokyo)
- **Performance Optimizations** (`scripts/client.sh`, logged at every boot with a `HUNT:` prefix):
  - Hyper-threading disabled (`threads_per_core = 1`)
  - CPU isolation: cores 2..N-1 isolated via kernel boot parameters (`isolcpus`, `nohz_full`, `rcu_nocbs`), set on first boot followed by one reboot; the benchmark is pinned to them
  - Housekeeping: cores 0-1 take all device interrupts (`irqaffinity`, irqbalance disabled), unbound kernel workqueues and system services
  - Noise reduction: `idle=poll`, `clocksource=tsc`, NMI watchdog, audit and THP off, socket busy polling, NIC interrupt moderation off
  - Turbo Boost and CPU governor: set only where the platform exposes them, which on EC2 means bare metal; elsewhere frequency is managed by the platform

> **Note**: The goal is measurement consistency — ensuring observed latency differences come from network path placement, not host noise such as scheduler, interrupt or idle-state jitter.

## Prerequisites

- Terraform >= 1.0
- AWS CLI configured with appropriate credentials
- [Session Manager plugin](https://docs.aws.amazon.com/systems-manager/latest/userguide/session-manager-working-with-install-plugin.html) for AWS CLI
- Rust toolchain (for local testing)
- jq (for parsing JSON outputs)

## Local Testing

Test the Rust WebSocket client locally before deployment:

```bash
cd sources/cex_ws

# Find CEX WebSocket IPs
dig +short <ws-fapi.cex-endpoint.com>

# Run with specific target IP
cargo run <target_ip_address>
```

The client will:
- Connect to the specified CEX IP
- Send orderbook depth requests every second
- Measure latency (min/max/avg/percentiles)
- Exit after 600 samples with statistics

## Deployment

### 1. Package Source Code

```bash
sh pack_source.sh
```

This creates `sources/source.tar.gz` containing the Rust project.

### 2. Configure Variables

Copy the example tfvars file and fill in your environment-specific values:

```bash
cp terraform.tfvars.example terraform.tfvars
```

Edit `terraform.tfvars` with your values:
- `aws_vpc_id`: Your target VPC ID
- `aws_availability_zone_id`: Target availability zone
- `s3_bucket`: S3 bucket for source code distribution
- `ami_id` (optional): the AMI to hunt with, see below

See `variables.tf` for all available options.

#### Hunt with your production AMI

Set `ami_id` to the AMI the winning instance will run in production (e.g. your latency-tuned golden AMI). An instance keeps its host across reboots, but switching to another AMI means launching a new instance, which lands on a new host and throws the hunted placement away. Without `ami_id`, the latest Amazon Linux 2023 AMI with kernel 6.18 is used.

- The AMI must be x86_64 with cloud-init, AWS CLI v2 (to fetch the source from S3) and SSM Agent (the only access path). `scripts/client.sh` is written for Amazon Linux 2023; on other distributions its tuning steps may fail.
- Existing instances ignore later changes to `ami_id` or to the default AMI. To re-hunt with a new AMI, run `terraform apply -replace='aws_instance.client_ppg["N"]'` per instance, or destroy and apply again.
- If the AMI already sets `isolcpus`, `client.sh` leaves the kernel command line unchanged and the benchmark runs on the AMI's isolated cores.

### 3. Deploy Infrastructure

```bash
terraform init
terraform apply --auto-approve
```

This provisions:
- Partition placement group with 7 partitions
- VPC subnet, internet gateway, and routing
- Security group (egress-only, no inbound ports)
- S3 VPC endpoint
- IAM roles and instance profiles
- EC2 instances with cloud-init scripts

On every boot each instance runs `scripts/client.sh`, which:
- applies the host tuning above; on first boot it sets the CPU isolation boot parameters and reboots once
- downloads the Rust source from S3 and builds it when the source changed

Wait until `grep HUNT: /var/log/cloud-init-output.log` shows `cmdline: active` (or `the AMI sets its own isolcpus`) before benchmarking.


### 4. Access Instances

Instances are accessed via AWS SSM Session Manager (no SSH keys or open ports needed).

Login to specific instances:

```bash
# List instance IDs
terraform output -json instance_id_and_partition_id_mapping

# Start a session to a specific instance
aws ssm start-session --target <instance-id> --region ap-northeast-1
```

Run the Rust program simultaneously on all instances:
```bash
TARGET_IP=<target_ip_address>

# Run on a single instance via SSM session
sudo nohup taskset -c "$(cat /sys/devices/system/cpu/isolated)" /share/bench/sources/cex_ws/target/release/cex_ws $TARGET_IP > "/home/ec2-user/$(ec2-metadata -i | awk '{print $2}').log" 2>&1 &

# Or run on all instances at once via SSM send-command
INSTANCE_IDS=$(terraform output -json instance_id_and_partition_id_mapping | jq -r 'keys[]')
aws ssm send-command \
  --targets "Key=instanceids,Values=$(echo $INSTANCE_IDS | tr ' ' ',')" \
  --document-name "AWS-RunShellScript" \
  --parameters "commands=['sudo nohup taskset -c \$(cat /sys/devices/system/cpu/isolated) /share/bench/sources/cex_ws/target/release/cex_ws $TARGET_IP > /home/ec2-user/\$(ec2-metadata -i | awk \"{print \\$2}\").log 2>&1 &']" \
  --region ap-northeast-1
```


### 5. View Results

Check benchmark logs on each instance:

```bash
# View running log
tail -f /home/ec2-user/$(ec2-metadata -i | awk '{print $2}').log

# View cloud-init logs
cat /var/log/cloud-init-output.log
```

### 6. Keep the Winner

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
- Later edits to `ami_id` or `scripts/` are ignored for existing instances, so re-applying doesn't replace them; they apply to newly launched instances only.
- To update the Rust client on running instances, run `sh pack_source.sh` and `terraform apply`, then reboot them: the boot script rebuilds when the source changed.
- `terraform destroy` removes everything, including the winner.
- Re-hunt periodically, as exchange endpoints and AWS capacity change over time: remove `keep_instances` and apply to launch fresh instances alongside the winner.

### 7. Destroy Infrastructure

```bash
terraform destroy --auto-approve
```

## Benchmark Configuration

The Rust client (`sources/cex_ws/src/main.rs`) performs:

1. **Connection**: Connects to specified CEX endpoint via WebSocket
2. **Warm-up**: Skips first 5 requests
3. **Measurement**: Records 600 samples with 1-second intervals
4. **Metrics**: Tracks min, max, average, and percentiles (p50, p75, p90, p99, p99.9)
5. **Request**: Depth data for BTCUSDT with 5 levels


## Outputs

- `instance_public_ip_client_ppg`: Public IPs of all instances
- `instance_id_and_partition_id_mapping`: Instance-to-partition mapping
- `instances`: Instance index (used by `keep_instances`) to instance ID and partition

## Troubleshooting

### Cloud-init Issues

```bash
cat /var/log/cloud-init-output.log
```

### Check Running Process

```bash
ps aux | grep cex_ws
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

## Project Structure

```
.
├── main.tf                    # Main Terraform configuration
├── moved.tf                   # State address migration for instances
├── variables.tf               # Variable definitions
├── providers.tf               # AWS provider configuration
├── terraform.tfvars.example   # Example variable values
├── pack_source.sh             # Script to package Rust source
├── tests/
│   └── hunt.tftest.hcl        # Offline `terraform test` checks (mock AWS)
├── scripts/
│   ├── client.sh              # EC2 initialization script
│   └── cloud-config.yaml      # Cloud-init configuration
└── sources/
    └── cex_ws/            # Rust WebSocket client
        ├── Cargo.toml
        └── src/
            └── main.rs        # Main benchmark logic
```

## Notes

- Benchmark does not auto-run; you need to manually start it via SSM (see step 4)
- Results are logged to `/home/ec2-user/<instance-id>.log`
- Process exits after 600 samples (~10 minutes)