.init_vf_all
--enter
in_vf a(vf01)
--endenter
.show a, b
add t, a, a
.hide u
add u, t, a
mul b, u, t
.endhide
.endshow
--exit
out_vf b(vf02)
--endexit
