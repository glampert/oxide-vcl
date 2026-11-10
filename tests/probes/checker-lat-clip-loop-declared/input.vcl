; repro__clip_loop_entry with the clip history declared as an input.
.name Repro_clip_loop_entry
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
in_hw_clip CLIP
--endenter
    lq fA, 1(vi00)
    iaddiu iN, vi00, 4
    div q, vf00[w], fA[w]
    clipw.xyz fA, fA[w]
lLoop:
    mulq fF, fA, q
    fcand vi01, 0x3f
    isw.x vi01, 5(vi00)
    sq fF, 2(vi00)
    iaddi iN, iN, -1
    div q, vf00[w], fF[w]
    clipw.xyz fF, fF[w]
    ibne iN, vi00, lLoop
--exit
--endexit
