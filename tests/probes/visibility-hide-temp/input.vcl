.init_vf_all
--enter
in_vf a(vf01)
--endenter
.hide t
add t, a, a
.endhide
add b, t, t
--exit
out_vf b(vf02)
--endexit
