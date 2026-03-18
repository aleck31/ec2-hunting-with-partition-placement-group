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
  vpc_id            = data.aws_vpc.existing.id
  cidr_block           = var.subnet_cidr_block
  availability_zone_id = var.aws_availability_zone_id

  tags = {
    Name = "main-public-snet"
  }
}

resource "aws_internet_gateway" "main" {
  vpc_id = data.aws_vpc.existing.id

  tags = {
    Name = "main-igw"
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
    gateway_id = aws_internet_gateway.main.id
  }

  tags = {
    Name = "main-rt"
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

data "aws_ami" "amazon_linux_2023" {
  most_recent = true
  owners      = ["amazon"]

  filter {
    name   = "name"
    values = ["al2023-ami-2023*-x86_64"]
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

resource "aws_instance" "client_ppg" {
  count                                = length(var.main_ec2_instance_type_client_ppg)
  ami                                  = data.aws_ami.amazon_linux_2023.image_id
  instance_type                        = var.main_ec2_instance_type_client_ppg[count.index]
  subnet_id                            = aws_subnet.public.id
  user_data                            = data.cloudinit_config.cloudinit_client.rendered
  user_data_replace_on_change          = true
  security_groups                      = [aws_security_group.main.id]
  iam_instance_profile                 = aws_iam_instance_profile.ec2_profile.name
  depends_on                           = [aws_route_table.rtb_public, aws_placement_group.partition]
  associate_public_ip_address          = true
  hibernation                          = false
  instance_initiated_shutdown_behavior = "terminate"
  placement_group                      = aws_placement_group.partition.name
  placement_partition_number = count.index + 1

  cpu_options {
    threads_per_core = 1
  }
  tags = {
    Name = "client-ppg-${count.index}"
  }
}

output "instance_public_ip_client_ppg" {
  value = [for s in aws_instance.client_ppg : s.public_ip]
}

output "instance_id_and_partition_id_mapping" {
  value = {
    for idx, instance in aws_instance.client_ppg : instance.id => instance.placement_partition_number
  }
}
