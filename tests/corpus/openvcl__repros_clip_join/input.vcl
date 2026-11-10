; oxide-vcl corpus program openvcl__repros_clip_join
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/clip_join.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_clip_join
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    ilw.x iFmt, 0(vi00)
    lq fA, 1(vi00)
    ibeq iFmt, vi00, lFmtB
    clipw.xyz fA, fA[w]
    b lJoin
lFmtB:
    clipw.xyz fA, fA[w]
lJoin:
    fcand vi01, 0x3f
    iaddiu iOut, vi01, 0
    isw.x iOut, 2(vi00)
--exit
--endexit
