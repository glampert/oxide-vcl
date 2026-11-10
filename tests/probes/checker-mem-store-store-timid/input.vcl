.name Repro_store_store_alias
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    iaddiu iWalk, vi00, 900
    lq fA, 1(vi00)
    lq fB, 2(vi00)
    mul fC, fA, fB
    sq fC, 1(iWalk)
    sq fA, 901(vi00)
--exit
--endexit
