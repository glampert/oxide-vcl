.init_vf_all
.init_vi_all
.syntax new
--enter
in_vf model[0](vf01)
in_hw_acc ACC
--endenter
madd result[0],model[0],model[0][w]
--exit
out_vf result[0](vf02)
--endexit
