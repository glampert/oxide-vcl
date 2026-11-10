.init_vf_all
--enter
in_vf vf01
in_vf vf02
in_vi vi01
--endenter
.vsmloop
loop:
--LoopCS 1,0
mul vf03,vf01,vf01 nop
mul vf04,vf03,vf01 isubiu vi01,vi01,1
add vf02,vf02,vf04 nop
nop ibne vi01,vi00,loop
nop nop
.endvsmloop
--exit
out_vf vf02
out_vi vi01
--endexit
