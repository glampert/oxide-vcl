.init_vf_all
.init_vi_all
--enter
in_vf vf01
in_vf vf02
in_vf vf03
in_vi vi01
--endenter
.vsmloop reorder
loop:
--LoopCS 1,0
1..mul vf03,vf01,vf01 1..mtir vi02,vf03x
2..mul vf04,vf03,vf01 1..isubiu vi01,vi01,1
3..add vf02,vf02,vf04 nop
nop 1..ibne vi01,vi00,loop
nop nop
.endvsmloop
--exit
out_vf vf02
out_vi vi01
out_vi vi02
--endexit
