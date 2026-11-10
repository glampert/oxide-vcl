.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     vi, a, a
        mul     b, vi, a
--exit
out_vf b(vf02)
--endexit
