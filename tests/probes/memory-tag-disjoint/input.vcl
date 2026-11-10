.init_vf_all
--enter
in_vi src(vi01)
in_vi dst(vi02)
in_vf a(vf01)
--endenter
        sq      a, 0(dst):out
        lq      b, 0(src):in
        add     c, b, b
--exit
out_vf c(vf03)
--endexit
