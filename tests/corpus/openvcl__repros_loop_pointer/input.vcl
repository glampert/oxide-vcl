; oxide-vcl corpus program openvcl__repros_loop_pointer
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/loop_pointer.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_loop_pointer
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    iaddiu iPtr, vi00, 100
    iaddiu iCount, vi00, 10
lLoop:
    ilw.x iCond, 0(iCount)
    ibeq iCond, vi00, lSkip
    iaddiu iTemp, vi00, 5
    isw.x iTemp, 1(vi00)
lSkip:
    isw.x iCount, 0(iPtr)
    iaddiu iPtr, iPtr, 1
    iaddiu iT2, vi00, 7
    iaddiu iT3, iT2, 1
    isw.x iT3, 2(vi00)
    iaddi iCount, iCount, -1
    ibne iCount, vi00, lLoop
--exit
--endexit
