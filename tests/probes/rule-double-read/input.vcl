.init_vf_all
--enter
in_vf a(vf01)
in_vf b(vf02)
--endenter
        add     sum, a, b
        add     dbl, b, b
        mul     r, sum, sum
        mul     s, dbl, a
--exit
out_vf r(vf03)
out_vf s(vf04)
--endexit
