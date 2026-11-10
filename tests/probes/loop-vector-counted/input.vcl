--enter
in_vf a(vf01)
in_vf b(vf02)
in_vi c(vi01)
--endenter
loop:
--LoopCS 1,0
add b,b,a
isubiu c,c,1
ibne c,vi00,loop
--exit
out_vf b(vf02)
out_vi c(vi01)
--endexit
