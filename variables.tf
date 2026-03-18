variable "aws_region" {
  description = "AWS region for deployment"
  type        = string
  default     = "ap-northeast-1"
}

variable "aws_vpc_id" {
  description = "ID of the existing VPC to deploy into"
  type        = string
}

variable "aws_availability_zone_id" {
  description = "Availability zone ID (e.g. apne1-az4)"
  type        = string
}

variable "subnet_cidr_block" {
  description = "CIDR block for the public subnet"
  type        = string
  default     = "10.0.10.0/24"
}

variable "main_ec2_instance_type_client_ppg" {
  description = "List of EC2 instance types for each partition (one per partition)"
  type        = list(string)
  default     = ["c6in.2xlarge", "c6in.2xlarge", "c6in.2xlarge", "c6in.2xlarge", "c6in.2xlarge", "c6in.2xlarge", "c6in.2xlarge"]
}

variable "partition_placement_group_name" {
  description = "Name of the partition placement group"
  type        = string
  default     = "ws-latency-partition-placement-group"
}

variable "s3_bucket" {
  description = "Existing S3 bucket name for source code distribution"
  type        = string
}

variable "ec2_iam_role_name" {
  description = "Name of the IAM role for EC2 S3 access"
  type        = string
  default     = "ws-latency-ec2-s3-access"
}

variable "s3_policy_name" {
  description = "Name of the S3 access IAM policy"
  type        = string
  default     = "ws-latency-s3-access-policy"
}

variable "ec2_instance_profile_name" {
  description = "Name of the EC2 instance profile"
  type        = string
  default     = "ws-latency-ec2-profile"
}