resource "aws_placement_group" "partition" {
  name            = var.partition_placement_group_name
  strategy        = "partition"
  partition_count = 7
}

data "aws_s3_bucket" "existing" {
  bucket = var.s3_bucket  # Replace with your actual bucket name
}

resource "aws_s3_object" "main_source" {
  bucket  = data.aws_s3_bucket.existing.bucket
  acl     = "private"
  key     = "source.tar.gz"
  source = "${path.module}/sources/source.tar.gz"
  etag = filemd5("${path.module}/sources/source.tar.gz")
  force_destroy = true
}

resource "aws_iam_role" "ec2_s3_access" {
  name = var.ec2_iam_role_name

  assume_role_policy = <<EOF
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Action": "sts:AssumeRole",
      "Principal": {
        "Service": "ec2.amazonaws.com"
      },
      "Effect": "Allow",
      "Sid": ""
    }
  ]
}
EOF
}

# Attach IAM policy to allow S3 access
resource "aws_iam_policy" "s3_access_policy" {
  name        = var.s3_policy_name
  path        = "/"
  description = "IAM policy for accessing S3 bucket"

  policy = <<EOF
{
  "Version": "2012-10-17",
  "Statement": [
    {
      "Action": [
        "s3:ListBucket"
      ],
      "Effect": "Allow",
      "Resource": "arn:aws:s3:::${data.aws_s3_bucket.existing.bucket}"
    },
    {
      "Action": [
        "s3:GetObject"
      ],
      "Effect": "Allow",
      "Resource": "arn:aws:s3:::${data.aws_s3_bucket.existing.bucket}/*"
    },
    {
      "Action": [
        "s3:PutObject"
      ],
      "Effect": "Allow",
      "Resource": "arn:aws:s3:::${data.aws_s3_bucket.existing.bucket}/results/*"
    }
  ]
}
EOF
}

resource "aws_iam_role_policy_attachment" "attach" {
  role       = aws_iam_role.ec2_s3_access.name
  policy_arn = aws_iam_policy.s3_access_policy.arn
}

resource "aws_iam_role_policy_attachment" "ssm" {
  role       = aws_iam_role.ec2_s3_access.name
  policy_arn = "arn:aws:iam::aws:policy/AmazonSSMManagedInstanceCore"
}

data "aws_vpc" "existing" {
  id = var.aws_vpc_id  # Replace with your actual VPC ID
}

resource "aws_subnet" "public" {
  vpc_id               = data.aws_vpc.existing.id
  cidr_block           = var.subnet_cidr_block
  availability_zone_id = var.aws_availability_zone_id

  tags = {
    Name = "ws-latency-subnet"
  }
}

# A VPC takes only one Internet Gateway, so reuse the existing one when given
resource "aws_internet_gateway" "main" {
  count  = var.internet_gateway_id == null ? 1 : 0
  vpc_id = data.aws_vpc.existing.id

  tags = {
    Name = "ws-latency-igw"
  }
}

resource "aws_route_table_association" "main" {
  subnet_id      = aws_subnet.public.id
  route_table_id = aws_route_table.rtb_public.id
}

resource "aws_route_table" "rtb_public" {
  vpc_id = data.aws_vpc.existing.id

  route {
    cidr_block = "0.0.0.0/0"
    gateway_id = var.internet_gateway_id != null ? var.internet_gateway_id : aws_internet_gateway.main[0].id
  }

  tags = {
    Name = "ws-latency-rt"
  }
}

resource "aws_security_group" "main" {
  name   = "ws-latency-sg"
  vpc_id = data.aws_vpc.existing.id

  egress {
    from_port   = 0
    to_port     = 0
    protocol    = "-1"
    cidr_blocks = ["0.0.0.0/0"]
  }
}

# Create an S3 VPC Endpoint
resource "aws_vpc_endpoint" "s3" {
  vpc_id          = data.aws_vpc.existing.id
  service_name    = "com.amazonaws.${var.aws_region}.s3"
  route_table_ids = [aws_route_table.rtb_public.id]
}

# Create instance profile and associate it with the IAM role
resource "aws_iam_instance_profile" "ec2_profile" {
  name = var.ec2_instance_profile_name
  role = aws_iam_role.ec2_s3_access.name
}

# Default AMI, used only when var.ami_id is unset; pins the kernel series so hunts don't flip between 6.1/6.12/6.18
data "aws_ami" "amazon_linux_2023" {
  count       = var.ami_id == null ? 1 : 0
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["al2023-ami-2023.*-kernel-6.18-x86_64"]
  }
}

// Incase to test arm device
# data "aws_ami" "amazon_linux_2023_arm" {
#   most_recent = true
#   owners      = ["amazon"]
#
#   filter {
#     name   = "name"
#     values = ["al2023-ami-2023*-arm64"]
#   }
# }

data "cloudinit_config" "cloudinit_client" {
  gzip          = false
  base64_encode = true

  part {
    filename     = "client.sh"
    content_type = "text/x-shellscript-per-boot"
    content      = templatefile("${path.module}/scripts/client.sh", {
      s3_bucket = var.s3_bucket
    })
  }

  part {
    filename     = "cloud-config.yaml"
    content_type = "text/cloud-config"

    content = file("${path.module}/scripts/cloud-config.yaml")
  }
}

locals {
  instance_indices = var.keep_instances != null ? var.keep_instances : range(var.instance_count + var.unplaced_instance_count)
}

data "aws_ec2_instance_type" "selected" {
  instance_type = var.instance_type
}

# Keyed by index ("0", "1", ...) so dropping losers via keep_instances doesn't shift the winner's address
resource "aws_instance" "client_ppg" {
  for_each                             = toset([for i in local.instance_indices : tostring(i)])
  ami                                  = var.ami_id != null ? var.ami_id : data.aws_ami.amazon_linux_2023[0].image_id
  instance_type                        = var.instance_type
  subnet_id                            = aws_subnet.public.id
  user_data                            = data.cloudinit_config.cloudinit_client.rendered
  vpc_security_group_ids               = [aws_security_group.main.id]
  iam_instance_profile                 = aws_iam_instance_profile.ec2_profile.name
  depends_on                           = [aws_route_table.rtb_public, aws_placement_group.partition]
  associate_public_ip_address          = true
  hibernation                          = false
  instance_initiated_shutdown_behavior = "terminate"
  # Indices from instance_count on launch outside the placement group, sampling hosts beyond its racks
  placement_group            = tonumber(each.key) < var.instance_count ? aws_placement_group.partition.name : null
  placement_partition_number = tonumber(each.key) < var.instance_count ? tonumber(each.key) % aws_placement_group.partition.partition_count + 1 : null

  # Hyper-threading off; without core_count the provider drops threads_per_core and launches with SMT on
  cpu_options {
    core_count       = data.aws_ec2_instance_type.selected.default_cores
    threads_per_core = 1
  }

  # Changing any of these relaunches or stop/starts the instance, which loses the hunted placement
  lifecycle {
    ignore_changes = [ami, user_data, instance_type, cpu_options, placement_group, placement_partition_number]
  }

  tags = {
    Name = tonumber(each.key) < var.instance_count ? "client-ppg-${each.key}" : "client-nopg-${each.key}"
  }
}

output "instance_public_ip_client_ppg" {
  value = [for s in aws_instance.client_ppg : s.public_ip]
}

output "s3_bucket" {
  description = "Bucket holding the client source and the results/ uploaded by scripts/hunt.py"
  value       = data.aws_s3_bucket.existing.bucket
}

output "aws_region" {
  value = var.aws_region
}

output "instances" {
  description = "Instance index (as used by keep_instances) => instance ID and partition (0 outside the placement group)"
  value = {
    for idx, instance in aws_instance.client_ppg : idx => {
      id                 = instance.id
      partition          = instance.placement_partition_number
      in_placement_group = tonumber(idx) < var.instance_count
    }
  }
}

output "instance_id_and_partition_id_mapping" {
  value = {
    for idx, instance in aws_instance.client_ppg : instance.id => instance.placement_partition_number
  }
}
