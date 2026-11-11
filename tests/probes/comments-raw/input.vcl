.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
--endenter
.raw
add vf03, vf00, vf00    iaddiu vi02, vi00, 5
nop                         nop
mul vf04, vf03, vf03    nop
.endraw
add res, vf04, src
--exit
out_vf res(vf02)
--endexit
