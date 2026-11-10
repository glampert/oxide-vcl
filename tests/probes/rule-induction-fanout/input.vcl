.init_vi_all
.init_vf_all
--enter
in_vi ptr(vi01)
in_vf a(vf01)
--endenter
        iaddiu  ptr, ptr, 7
        sq      a, 0(ptr)
        sq      a, 1(ptr)
        sq      a, 2(ptr)
        sq      a, 3(ptr)
        add     b, a, a
        mul     c, b, b
--exit
out_vf c(vf02)
out_vi ptr(vi01)
--endexit
