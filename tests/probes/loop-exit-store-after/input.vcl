.init_vf_all
.init_vi_all
--enter
in_vi base(vi01)
in_vi count(vi02)
--endenter
loop:
--LoopCS 1,0
lq value,0(base)
mul value,value,value
sq value,0(base)
iaddiu base,base,1
isubiu count,count,1
ibne count,vi00,loop
sq value,1(base)
--exit
--endexit
