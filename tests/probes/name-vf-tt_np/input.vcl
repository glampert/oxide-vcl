.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     tt_np, a, a
        mul     b, tt_np, a
--exit
out_vf b(vf02)
--endexit
