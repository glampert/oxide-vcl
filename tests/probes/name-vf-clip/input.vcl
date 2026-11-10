.init_vf_all
--enter
in_vf a(vf01)
--endenter
        add     clip, a, a
        mul     b, clip, a
--exit
out_vf b(vf02)
--endexit
