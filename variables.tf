variable "aws_region" {
  type = string
  default = "ap-northeast-1"
}

variable "aws_vpc_id" {
  type = string
  default = "vpc-020b9d1be982164bf"
}

variable "aws_availability_zone_id" {
  type = string
  default = "apne1-az4"
}

variable "main_ec2_instance_type_client_ppg" {
  type = list(string)
  # default = ["c6in.2xlarge"]
  default = ["c6in.2xlarge","c6in.2xlarge","c6in.2xlarge","c6in.2xlarge","c6in.2xlarge","c6in.2xlarge","c6in.2xlarge"]
}

variable "partition_placement_group_name" {
  type = string
  default = "binance-test-partition-placement-group"
}

variable "s3_bucket" {
  type = string
  default = "garyyhlo-ap-northeast-1"
}

variable "ec2_iam_role_name" {
  type = string
  default = "binance-test-ec2-s3-access"
}

variable "s3_policy_name" {
  type = string
  default = "binance-test-s3-access-policy"
}
variable "ec2_instance_profile_name" {
  type = string
  default = "binance-test-ec2-profile"
}

variable "ssh_key_name" {
  type = string
  default = "binance-test-ssh-key"
}