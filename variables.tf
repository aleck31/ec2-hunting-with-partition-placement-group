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

variable "instance_type" {
  description = "Instance type and size to hunt with; use the production one, since changing it later needs a stop/start that loses the placement"
  type        = string
  default     = "c8i.2xlarge"
}

variable "instance_count" {
  description = "Number of instances to launch, spread round-robin over the 7 partitions; more instances, more placements sampled"
  type        = number
  default     = 7

  validation {
    condition     = var.instance_count >= 1
    error_message = "instance_count must be at least 1."
  }
}

variable "internet_gateway_id" {
  description = "Existing Internet Gateway of the VPC to route through; null creates one (fails if the VPC already has one)"
  type        = string
  default     = null
}

variable "ami_id" {
  description = "AMI to hunt with; set it to your production AMI (x86_64). Null uses the latest AL2023 kernel-6.18 AMI"
  type        = string
  default     = null
}

variable "keep_instances" {
  description = "After hunting, indices of the instances to keep (e.g. [3]); the others are terminated. Null keeps all"
  type        = list(number)
  default     = null
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