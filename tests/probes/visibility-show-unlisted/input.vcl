.init_vf_all
--enter
in_vf a(vf01)
--endenter
.show b
add t, a, a
mul b, t, t
.endshow
--exit
out_vf b(vf02)
--endexit
