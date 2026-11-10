.init_vi VI02, VI03, VI04, VI05
.init_vf_all
.syntax new
--enter
--endenter
xtop iPacket
xgkick iPacket
iaddiu iNullTag, vi00, 8
isw.x iNullTag, 0(vi00)
--exit
--endexit
