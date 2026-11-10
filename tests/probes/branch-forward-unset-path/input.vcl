--enter
in_vf a(vf01)
in_vi c(vi01)
--endenter
ibeq c,vi00,done
mul b,a,a
done:
--exit
out_vf b(vf02)
--endexit
