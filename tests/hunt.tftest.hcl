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
}

variables {
  aws_vpc_id               = "vpc-0123456789abcdef0"
  aws_availability_zone_id = "apne1-az4"
  s3_bucket                = "bucket"
}

run "hunt" {
  assert {
    condition     = [for k in sort(keys(output.instances)) : output.instances[k].partition] == [1, 2, 3, 4, 5, 6, 7]
    error_message = "Expected one instance in each of the 7 partitions"
  }
}

run "ami_change_keeps_instances" {
  command = plan
  variables {
    ami_id = "ami-0123456789abcdef0"
  }
  # The mock provider doesn't model ForceNew, so check that the planned AMI stays at the deployed one
  assert {
    condition     = aws_instance.client_ppg["0"].ami != "ami-0123456789abcdef0"
    error_message = "Changing the AMI would replace hunted instances"
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
