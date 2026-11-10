.init_vf_all
--enter
in_vf a(vf01)
--endenter
.hide t
add t, a, a
mul b, t, t
.endhide
.hide t
add t, b, b
mul c, t, t
.endhide
--exit
out_vf c(vf02)
--endexit
