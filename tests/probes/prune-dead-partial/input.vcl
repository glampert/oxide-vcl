.init_vf_all
--enter
in_vf.x a(vf01)
in_vf c(vf03)
--endenter
mul unused,a,c
add.x b,a,a
--exit
out_vf.x b(vf02)
--endexit
