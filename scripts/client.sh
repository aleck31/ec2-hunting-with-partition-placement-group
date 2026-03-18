#!/bin/bash
VERSION='v1'
TIMESTAMP=$(date +"%s")

# Disable hyper-threading
# https://aws.amazon.com/blogs/compute/disabling-intel-hyper-threading-technology-on-amazon-linux/
for cpunum in $(cat /sys/devices/system/cpu/cpu*/topology/thread_siblings_list | cut -s -d, -f2- | tr ',' '\n' | sort -un)
do
	sudo echo 0 > /sys/devices/system/cpu/cpu$cpunum/online
done

# Disable Turbo Boost (1 = turbo disabled) to ensure consistent CPU frequency,
# so that latency differences reflect network path, not CPU frequency fluctuation.
# https://docs.aws.amazon.com/AWSEC2/latest/UserGuide/processor_state_control.html
sudo sh -c "echo 1 > /sys/devices/system/cpu/intel_pstate/no_turbo"
sudo cpupower frequency-set --governor performance

mkdir -p /share \
&& curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs -o /share/rust-init.sh \
&& env RUSTUP_HOME=/share/.rustup CARGO_HOME=/share/.cargo sh /share/rust-init.sh -y \
&& ln -s /share/.cargo/bin/* /usr/bin/ \
&& sudo /share/.cargo/bin/rustup default stable \
&& aws s3 cp s3://${s3_bucket}/source.tar.gz /share/source.tar.gz \
&& mkdir -p /share/bench \
&& tar -zxf /share/source.tar.gz -C /share/bench \
&& cd /share/bench/sources/cex_ws \
&& sudo /share/.cargo/bin/cargo build --release

# Isolate work from kernal thread
sudo yum install -y https://mirror.stream.centos.org/9-stream/BaseOS/x86_64/os/Packages/tuna-0.19-4.el9.noarch.rpm
sudo tuna isolate --cpus=0-1

### If you want to run it once the init ready
#TARGET_IP=54.168.162.131
#sudo nohup taskset -c 0-1 /share/bench/sources/cex_ws/target/release/cex_ws $TARGET_IP > "/home/ec2-user/$(ec2-metadata -i | awk '{print $2}').log" 2>&1 &