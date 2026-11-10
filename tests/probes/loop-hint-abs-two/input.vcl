.init_vf_all
.init_vi_all
--enter
in_vi src(vi01)
in_vi count(vi02)
--endenter
loop:
--LoopCS 1,0
--LoopAbs 2
isubiu count, count, 1
ibne count, vi00, loop
--exit
--endexit
