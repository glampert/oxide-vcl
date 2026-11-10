.init_vf_all
--enter
in_vf vf01
--endenter
mul temp,vf01,vf01
add first,temp,vf01
sub temp,vf01,vf01
add second,temp,vf01
--exit
out_vf first(vf02)
out_vf second(vf02)
--endexit
