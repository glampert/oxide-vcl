.init_vf_all
.init_vi_all
.syntax new
--enter
in_vf model[0](vf01)
--endenter
mtir result,model[0][w]
--exit
out_vi result(vi02)
--endexit
