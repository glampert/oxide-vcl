.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     q1, a, a
        mul     b, q1, a
--exit
out_vf b(vf02)
--endexit
