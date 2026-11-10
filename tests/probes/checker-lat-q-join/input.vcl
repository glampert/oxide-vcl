; DIV in one block, MULQ after a join one path reaches quickly.
--enter
in_vf a(vf01)
in_vi c(vi02)
--endenter
div Q,a.x,a.y
ibeq c,vi00,skip
iaddiu c,c,1
skip:
mulq b,a,Q
--exit
out_vf b(vf02)
out_vi c(vi02)
--endexit
