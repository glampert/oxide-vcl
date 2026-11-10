; DIV before a call whose subroutine reads Q first thing.
.init_vf_all
.init_vi_all
--enter
in_vf a(vf01)
--endenter
div Q,a.x,a.y
bal vi15,subroutine
back:
--exit
out_vf b(vf02)
--endexit
subroutine:
mulq b,a,Q
jr vi15
