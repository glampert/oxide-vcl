.init_vf_all
.init_vi_all
--enter
in_vf original(vf01)
--endenter
add argument,original,original
bal vi01,subroutine
first_return:
mul preserved,result,original
sub argument,original,result
bal vi01,subroutine
second_return:
add final,preserved,result
--exit
out_vf final(vf02)
--endexit
subroutine:
mul result,argument,argument
jr vi01
