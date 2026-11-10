.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     stat_div_s, a, a
        mul     b, stat_div_s, a
--exit
out_vf b(vf02)
--endexit
