.init_vf_all
--enter
in_vf a(vf01)
--endenter
.show a, b, t
add t, a, a
.show t, b
add u, t, t
mul b, u, u
.endshow
.endshow
--exit
out_vf b(vf02)
--endexit
