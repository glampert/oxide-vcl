.init_vf_all
.global live
.name AUDIT
--enter
in_vf a(vf01)
--endenter
live:
mul mid,a,a
--barrier
unused:
add result,mid,a
--exit
out_vf result(vf02)
--endexit
