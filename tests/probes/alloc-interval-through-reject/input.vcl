.init_vf vf03
--enter
in_vf a(vf01)
in_vi condition(vi01)
in_vi pointer(vi02)
--endenter
mul carried,a,a
ibeq condition,vi00,done
add scratch_temp,a,a
sq scratch_temp,0(pointer)
done:
add result,carried,a
--exit
out_vf result(vf02)
--endexit
