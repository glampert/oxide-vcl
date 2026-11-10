.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     Acc, a, a
        mul     b, Acc, a
--exit
out_vf b(vf02)
--endexit
