.init_vf vf03
--enter
in_vf a(vf01)
in_vi condition(vi01)
--endenter
ibeq condition,vi00,right
mul left_temp,a,a
add result,left_temp,a
b done
right:
add right_temp,a,a
mul result,right_temp,a
done:
--exit
out_vf result(vf02)
--endexit
