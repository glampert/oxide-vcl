; Flag immediates: FC* take 24 bits, FS* 12 bits.
--enter
in_vf a(vf01)
in_hw_clip CLIP
--endenter
fcand vi01,0x1000001
add vf02,a,a
fsand vi02,0x1001
fcset 0x1000002
--exit
out_vi vi01
out_vi vi02
out_vf vf02
out_hw_clip CLIP
--endexit
