--enter
in_vf a(vf01)
--endenter
div Q,a.x,a.y
waitq
mulq b,a,Q
--exit
out_vf b(vf02)
--endexit
