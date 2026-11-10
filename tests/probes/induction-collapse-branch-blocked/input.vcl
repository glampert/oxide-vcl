--enter
in_vi counter(vi01)
--endenter
isubiu counter,counter,1
ibeq counter,vi00,done
isubiu counter,counter,1
done:
--exit
out_vi counter(vi01)
--endexit
