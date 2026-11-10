.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     stat_alu, a, a
        mul     b, stat_alu, a
--exit
out_vf b(vf02)
--endexit
