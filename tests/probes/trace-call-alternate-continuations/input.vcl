.init_vf_all
.init_vi_all
--enter
in_vf input(vf01)
in_vi selector(vi02)
--endenter
ibeq selector,vi00,second_call
add argument,input,input
mul first_saved,input,input
bal vi01,subroutine
first_return:
add result,first_saved,returned
b finish
second_call:
sub argument,input,input
add second_saved,input,input
bal vi01,subroutine
second_return:
sub result,second_saved,returned
b finish
finish:
--exit
out_vf result(vf02)
--endexit
subroutine:
mul returned,argument,argument
jr vi01
