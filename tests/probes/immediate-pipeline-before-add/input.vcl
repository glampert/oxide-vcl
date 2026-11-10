.init_vf_all
--enter
in_vf vf01
in_vi vi01
in_vi vi02
--endenter
.vsmloop
loop:
--LoopCS 1,0
1..abs vf00,vf01 2..lq vf03,5(vi02)
nop 1..isubiu vi01,vi01,1
3..add vf02,vf03,vf01 1..iaddiu vi02,vi02,3
nop 1..ibne vi01,vi00,loop
nop nop
.endvsmloop
--exit
out_vf vf02
out_vi vi01
out_vi vi02
--endexit
