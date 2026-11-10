.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     clipa, a, a
        mul     b, clipa, a
--exit
out_vf b(vf02)
--endexit
