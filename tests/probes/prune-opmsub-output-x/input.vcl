--enter
in_vf a(vf01)
in_vf c(vf03)
in_hw_acc.xyz ACC
--endenter
opmsub.xyz b,a,c
--exit
out_vf.x b(vf02)
--endexit
