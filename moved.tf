# Maps earlier state addresses (count-based instances, the unconditional IGW) so existing deployments are not replaced
moved {
  from = aws_instance.client_ppg[0]
  to   = aws_instance.client_ppg["0"]
}

moved {
  from = aws_instance.client_ppg[1]
  to   = aws_instance.client_ppg["1"]
}

moved {
  from = aws_instance.client_ppg[2]
  to   = aws_instance.client_ppg["2"]
}

moved {
  from = aws_instance.client_ppg[3]
  to   = aws_instance.client_ppg["3"]
}

moved {
  from = aws_instance.client_ppg[4]
  to   = aws_instance.client_ppg["4"]
}

moved {
  from = aws_instance.client_ppg[5]
  to   = aws_instance.client_ppg["5"]
}

moved {
  from = aws_instance.client_ppg[6]
  to   = aws_instance.client_ppg["6"]
}

moved {
  from = aws_internet_gateway.main
  to   = aws_internet_gateway.main[0]
}
