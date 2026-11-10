.init_vi_all
.init_vf_all
--enter
in_vi flag(vi01)
in_vf a(vf01)
--endenter
        iaddiu  t, vi00, 1
        iand    t, flag, t
        ibeq    t, vi00, next
next:
        add     b, a, a
--exit
out_vf b(vf02)
--endexit
