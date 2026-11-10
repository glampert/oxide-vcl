.init_vf_all
--enter
in_vf a(vf01)
in_vf t(vf03)
--endenter
.show a, b
        add     t, a, a         ; the macro's own temporary
        mul     b, t, t
.endshow
        add     c, b, t         ; the caller's t, from vf03
--exit
out_vf c(vf02)
--endexit
