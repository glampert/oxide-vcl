; oxide-vcl corpus program doom3-bfg-ps2__src_ps2_renderer_vu1progs_passthrough
; From https://github.com/glampert/doom3-bfg-ps2 at adc427d: src/ps2/renderer/vu1progs/passthrough.vcl
; Produced: vclpp -I src/ps2/renderer/vu1progs -Wundef -Werror -j (vclpp 95911cd)
; License: GPL-3.0-or-later, see tests/corpus/licenses/GPL-3.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --

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

