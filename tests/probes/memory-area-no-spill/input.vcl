.vu_work_area vu1,0,32
--enter
in_vf a(vf01)
--endenter
mul c,a,a
add b,c,a
--exit
out_vf b(vf02)
--endexit
