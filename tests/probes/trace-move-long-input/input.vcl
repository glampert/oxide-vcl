.init_vf_all
.init_vi_all
--enter
in_vf seed(vf01)
in_vf bias(vf02)
in_vi count(vi01)
--endenter
loop:
--LoopCS 1,0
add root,seed,bias
mul chain,root,bias
add chain,chain,bias
mul chain,chain,bias
add chain,chain,bias
mul chain,chain,bias
add result,chain,root
isubiu count,count,1
ibne count,vi00,loop
--exit
out_vf result(vf03)
out_vi count(vi01)
--endexit
