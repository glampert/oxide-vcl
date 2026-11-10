--enter
in_vf a(vf01)
in_vi flag(vi03)
--endenter
ibeq flag,vi00,done
add vf02,a,a
done:
fsand vi01,0x400
--exit
out_vi vi01
--endexit
