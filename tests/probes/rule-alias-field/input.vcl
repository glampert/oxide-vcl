.init_vi_all
.init_vf_all
--enter
in_vi base(vi01)
in_vf a(vf01)
--endenter
        ilw.w   n, 3(base)
        mul     c, a, a
        sq      c, 3(base)
        iaddiu  m, n, 1
--exit
out_vi m(vi02)
--endexit
