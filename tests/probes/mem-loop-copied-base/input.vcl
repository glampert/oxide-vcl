; A pipelined loop that stores a product and loads it back through a copy of the base.
.init_vf_all
.init_vi_all
--enter
in_vi base(vi01)
in_vi count(vi02)
--endenter
    iaddiu iCopy, base, 0
    lq fB, 2(vi00)
top:
    --LoopCS 6, 1
    lq fA, 0(base)
    mul fC, fA, fB
    sq fC, 100(base)
    lq fD, 100(iCopy)
    add fE, fD, fD
    sq fE, 200(base)
    iaddiu base, base, 1
    iaddiu iCopy, iCopy, 1
    isubiu count, count, 1
    ibne count, vi00, top
--exit
--endexit
