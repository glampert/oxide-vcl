.init_vf_all
.init_vi_all
--enter
in_vf src(vf01)
in_vi flag(vi01)
--endenter
mul res, src, src
ibeq flag, vi00, skip
add res, res, src
skip:
--exit
out_vf res(vf02)
--endexit
