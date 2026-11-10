; oxide-vcl corpus program tyra__engine_src_renderer_core_paths_path1_draw_finish
; From https://github.com/h4570/tyra at c59f406: engine/src/renderer/core/paths/path1/draw_finish.vclpp
; Produced: vclpp, run from engine/
; License: Apache-2.0, see tests/corpus/licenses/Apache-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --

.name VU1DrawFinish
.syntax new
.name VU1DrawFinish
.vu
.init_vf_all
.init_vi_all
--enter
--endenter
    xtop    buffer
    iaddiu  kickAddress,    buffer, 10
    xgkick  kickAddress
--exit
--endexit
