; A store and a load at the same address through a base and its copy.
.init_vi_all
.init_vf_all
--enter
in_vi base(vi01)
--endenter
    iaddiu iCopy, base, 0
    lq fA, 1(vi00)
    lq fB, 2(vi00)
    mul fC, fA, fB
    sq fC, 7(base)
    lq fD, 7(iCopy)
    sq fD, 5(vi00)
--exit
--endexit
