; DIV at the bottom of a loop, MULQ at the top of the next iteration.
.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
in_vi n(vi01)
in_vi p(vi02)
--endenter
div Q,a.x,a.y
loop:
--LoopCS 1,0
mulq b,a,Q
sq b,0(p)
iaddiu p,p,1
lq a,0(p)
div Q,a.x,a.y
isubiu n,n,1
ibne n,vi00,loop
--exit
--endexit
