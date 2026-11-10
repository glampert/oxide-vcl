.global entry
.unique Local.Name
entry:
--enter
in_vi count(vi01)
--endenter
loop:
isubiu count,count,1
ibne count,vi00,loop
--exit
out_vi count(vi01)
--endexit
