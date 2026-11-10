.init_vf_all
.init_vi_all
--enter
in_vi   src(vi01)
in_vi   dst(vi02)
in_vi   count(vi03)
in_vf   scale(vf02)
--endenter
loop:
--LoopCS 4, 0
        lq      v, 0(src):buf
        mul     v, v, scale
        sq      v, 0(dst):buf
        iaddiu  src, src, 1
        iaddiu  dst, dst, 1
        isubiu  count, count, 1
        ibne    count, vi00, loop
--exit
--endexit
