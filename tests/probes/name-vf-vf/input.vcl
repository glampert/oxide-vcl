.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     vf, a, a
        mul     b, vf, a
--exit
out_vf b(vf02)
--endexit
