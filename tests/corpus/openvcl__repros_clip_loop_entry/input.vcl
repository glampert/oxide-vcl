; oxide-vcl corpus program openvcl__repros_clip_loop_entry
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/clip_loop_entry.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_clip_loop_entry
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    lq fA, 1(vi00)
    iaddiu iN, vi00, 4
    div q, vf00[w], fA[w]
    clipw.xyz fA, fA[w]
lLoop:
    mulq fF, fA, q
    fcand vi01, 0x3f
    isw.x vi01, 5(vi00)
    sq fF, 2(vi00)
    iaddi iN, iN, -1
    div q, vf00[w], fF[w]
    clipw.xyz fF, fF[w]
    ibne iN, vi00, lLoop
--exit
--endexit
