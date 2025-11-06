```shell
terraform apply --auto-approve
 
terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[0]' | tr -d '"')


terraform apply --auto-approve --destroy && terraform apply --auto-approve \
&& terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[0]' | tr -d '"')


tail -f /var/log/cloud-init-output.log


terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[0]' | tr -d '"')

terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[1]' | tr -d '"')

terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[2]' | tr -d '"')

terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[3]' | tr -d '"')

terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[4]' | tr -d '"')

terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[5]' | tr -d '"')


terraform output -raw private_key > priv.pem \
&& chmod 600 priv.pem \
&& ssh -oStrictHostKeyChecking=no -i priv.pem ec2-user@$(terraform output -json instance_public_ip_client_ppg | jq '.[6]' | tr -d '"')
```