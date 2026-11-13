; A pipelined loop that stores a product and loads it back through the same base.
.init_vf_all
.init_vi_all
--enter
in_vi base(vi01)
in_vi count(vi02)
--endenter
    lq fB, 2(vi00)
top:
    --LoopCS 6, 1
    lq fA, 0(base)
    mul fC, fA, fB
    sq fC, 100(base)
    lq fD, 100(base)
    add fE, fD, fD
    sq fE, 200(base)
    iaddiu base, base, 1
    isubiu count, count, 1
    ibne count, vi00, top
--exit
--endexit
