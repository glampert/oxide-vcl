.init_vf vf10
--enter
in_vf a(vf01)
--endenter
add t, a, a
mul u, a, a
add b, t, u
--exit
out_vf b(vf02)
--endexit
