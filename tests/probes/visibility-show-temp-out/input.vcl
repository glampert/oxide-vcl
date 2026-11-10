.init_vf_all
--enter
in_vf a(vf01)
--endenter
.show a
add t, a, a
.endshow
mul b, t, t
--exit
out_vf b(vf02)
--endexit
