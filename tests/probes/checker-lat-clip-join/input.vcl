; CLIPW in one block, FCAND after a join one path reaches quickly.
--enter
in_vf v(vf01)
in_vi c(vi02)
in_hw_clip CLIP
--endenter
clipw.xyz v,v.w
ibeq c,vi00,skip
iaddiu c,c,1
skip:
fcand vi01,0x3f
--exit
out_vi vi01
out_vi c(vi02)
--endexit
