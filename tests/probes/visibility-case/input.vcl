.init_vf_all
--enter
in_vf a(vf01)
--endenter
.hide T
add t, a, a
mul b, t, t
.endhide
--exit
out_vf b(vf02)
--endexit
