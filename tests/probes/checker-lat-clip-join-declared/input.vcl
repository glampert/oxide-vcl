; repro__clip_join with the clip history declared as an input.
.name Repro_clip_join
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
in_hw_clip CLIP
--endenter
    ilw.x iFmt, 0(vi00)
    lq fA, 1(vi00)
    ibeq iFmt, vi00, lFmtB
    clipw.xyz fA, fA[w]
    b lJoin
lFmtB:
    clipw.xyz fA, fA[w]
lJoin:
    fcand vi01, 0x3f
    iaddiu iOut, vi01, 0
    isw.x iOut, 2(vi00)
--exit
--endexit
