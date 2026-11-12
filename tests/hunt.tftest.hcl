# Offline checks (mock AWS provider) that re-applying never replaces hunted instances
mock_provider "aws" {
  mock_resource "aws_iam_policy" {
    defaults = { arn = "arn:aws:iam::123456789012:policy/s3" }
  }
  mock_resource "aws_iam_role" {
    defaults = { arn = "arn:aws:iam::123456789012:role/ec2" }
  }
  mock_resource "aws_iam_instance_profile" {
    defaults = { arn = "arn:aws:iam::123456789012:instance-profile/ec2" }
  }
  mock_data "aws_ec2_instance_type" {
    defaults = { default_cores = 4 }
  }
}

variables {
  aws_vpc_id               = "vpc-0123456789abcdef0"
  aws_availability_zone_id = "apne1-az4"
  s3_bucket                = "bucket"
}

run "hunt" {
  assert {
    condition     = aws_instance.client_ppg["0"].cpu_options[0].core_count == 4 && aws_instance.client_ppg["0"].cpu_options[0].threads_per_core == 1
    error_message = "cpu_options must set core_count with threads_per_core, or SMT stays on"
  }
  assert {
    condition     = [for k in sort(keys(output.instances)) : output.instances[k].partition] == [1, 2, 3, 4, 5, 6, 7]
    error_message = "Expected one instance in each of the 7 partitions"
  }
}

run "input_changes_keep_instances" {
  command = plan
  variables {
    ami_id        = "ami-0123456789abcdef0"
    instance_type = "c7i.2xlarge"
  }
  # The mock provider doesn't model ForceNew or stop/start, so check that planned values stay at the deployed ones
  assert {
    condition     = aws_instance.client_ppg["0"].ami != "ami-0123456789abcdef0"
    error_message = "Changing the AMI would replace hunted instances"
  }
  assert {
    condition     = aws_instance.client_ppg["0"].instance_type == "c8i.2xlarge"
    error_message = "Changing the instance type would stop/start hunted instances"
  }
}

run "partitions_wrap_and_unplaced_follow" {
  command = plan
  variables {
    instance_count          = 9
    unplaced_instance_count = 2
  }
  assert {
    condition     = aws_instance.client_ppg["7"].placement_partition_number == 1 && aws_instance.client_ppg["8"].placement_partition_number == 2
    error_message = "Instances beyond 7 must wrap around the partitions"
  }
  assert {
    condition     = output.instances["8"].in_placement_group && !output.instances["9"].in_placement_group && !output.instances["10"].in_placement_group
    error_message = "Unplaced instances must take the indices after instance_count, outside the group"
  }
}

run "keep_winner" {
  variables {
    keep_instances = [3]
  }
  assert {
    condition     = keys(output.instances) == ["3"] && output.instances["3"].id == run.hunt.instances["3"].id
    error_message = "keep_instances must keep only the winner, unchanged"
  }
}
