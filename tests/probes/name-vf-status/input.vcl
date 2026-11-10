.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     status, a, a
        mul     b, status, a
--exit
out_vf b(vf02)
--endexit
