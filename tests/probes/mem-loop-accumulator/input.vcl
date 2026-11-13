; A pipelined loop that keeps a sum at a fixed qword.
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
    lq fS, 50(vi00)
    add fS, fS, fA
    sq fS, 50(vi00)
    iaddiu base, base, 1
    isubiu count, count, 1
    ibne count, vi00, top
--exit
--endexit
