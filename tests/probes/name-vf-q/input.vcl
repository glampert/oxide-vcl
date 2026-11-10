.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     q, a, a
        mul     b, q, a
--exit
out_vf b(vf02)
--endexit
