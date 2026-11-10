.init_vf_all
--enter
in_vf _a(vf01)
--endenter
.hide _a
add b, _a, _a
.endhide
--exit
out_vf b(vf02)
--endexit
