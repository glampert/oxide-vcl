.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     ramreg, a, a
        mul     b, ramreg, a
--exit
out_vf b(vf02)
--endexit
