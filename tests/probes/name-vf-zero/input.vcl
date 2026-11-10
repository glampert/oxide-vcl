.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     zero, a, a
        mul     b, zero, a
--exit
out_vf b(vf02)
--endexit
