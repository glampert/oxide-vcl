
.name passthrough

.init_vi VI02, VI03, VI04, VI05, VI06, VI07, VI08, VI09, VI10, VI11, VI12, VI13, VI14
.init_vf_all
.syntax new
.vu

--enter
--endenter

    xtop   iPacket
    xgkick iPacket
    iaddiu iNullTag, vi00, 8
    xgkick iNullTag

--exit
--endexit

