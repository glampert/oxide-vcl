.init_vf_all
--enter
in_vi src(vi01)
in_vi dst(vi02)
in_vf a(vf01)
--endenter
        sq      a, 0(dst):buf
        lq      b, 0(src):buf
        add     c, b, b
--exit
out_vf c(vf03)
--endexit
