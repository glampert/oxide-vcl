.init_vi vi01-vi03
.vu_work_area vu1,1000,8
--enter
in_vi base(vi10)
--endenter
iaddiu ca, base, 1
iaddiu cb, base, 2
iaddiu cc, base, 3
iaddiu cd, base, 4
iadd sa, ca, cb
iadd sb, cc, cd
iadd sc, sa, ca
iadd sd, sb, cd
iadd tot, sc, sd
--exit
out_vi tot(vi11)
--endexit
