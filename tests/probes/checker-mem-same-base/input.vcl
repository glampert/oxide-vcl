; The same store and load through one base register.
.init_vi_all
.init_vf_all
--enter
in_vi base(vi01)
--endenter
    lq fA, 1(vi00)
    lq fB, 2(vi00)
    mul fC, fA, fB
    sq fC, 7(base)
    lq fD, 7(base)
    sq fD, 5(vi00)
--exit
--endexit
