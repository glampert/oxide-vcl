--enter
in_vi c(vi01)
--endenter
loop:
isubiu c,c,1
ibne c,vi00,loop
--exit
out_vi c(vi01)
--endexit
