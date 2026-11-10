.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     R, a, a
        mul     b, R, a
--exit
out_vf b(vf02)
--endexit
