.init_vf_all
.init_vi_all
.syntax new
--enter
in_vf vf01
in_vf vf02
in_vi vi01
in_vi vi02
in_vi vi03
in_hw_acc ACC
in_hw_i I
in_hw_q Q
in_hw_p P
in_hw_r R
in_hw_clip CLIP
--endenter
maxx vf04,vf01,vf02[x]
--exit
out_vf vf04
--endexit
