.init_vf_all
--enter
in_vi src(vi01)
in_vi dst(vi02)
in_vf a(vf01)
--endenter
        sq.x    a, 0(dst):buf
        lq.y    b, 0(src):buf
        add.y   c, b, b
--exit
out_vf.y c(vf03)
--endexit
