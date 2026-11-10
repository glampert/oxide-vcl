.init_vf_all
--enter
in_vi src(vi01)
in_vi dst(vi02)
in_vf a(vf01)
--endenter
        mul     c, a, a
        mul     c, c, c
        sq      c, 0(dst):buf
        sq      a, 0(src):buf
--exit
--endexit
