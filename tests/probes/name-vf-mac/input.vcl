.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     mac, a, a
        mul     b, mac, a
--exit
out_vf b(vf02)
--endexit
