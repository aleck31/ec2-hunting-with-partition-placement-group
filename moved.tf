# Maps count-style addresses (client_ppg[N]) to for_each keys so deployments made with count are not replaced
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
