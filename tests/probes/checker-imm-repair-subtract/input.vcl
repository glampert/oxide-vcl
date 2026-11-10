.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
in_vf b(vf02)
in_vi count(vi01)
in_vi p(vi02)
--endenter
loop:
--LoopCS 1,0
--LoopAbs 5
isubiu p,p,3
lq c,-1023(p)
mul d,c,a
add b,b,d
isubiu count,count,1
ibne count,vi00,loop
--exit
out_vf b(vf02)
out_vi count(vi01)
out_vi p(vi02)
--endexit
