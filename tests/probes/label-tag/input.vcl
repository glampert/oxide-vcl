.init_vi_all
.init_vf_all
--enter
in_vi address(vi01)
--endenter
jr address:done
done:
--exit
--endexit
--enter
in_vi p(vi01)
--endenter
iaddiu p,p,1
--exit
out_vi p(vi01)
--endexit
