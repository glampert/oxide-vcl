; oxide-vcl corpus program openvcl__repros_store_store_alias
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/store_store_alias.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_store_store_alias
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    iaddiu iWalk, vi00, 900
    lq fA, 1(vi00)
    lq fB, 2(vi00)
    mul fC, fA, fB
    sq fC, 1(iWalk)
    sq fA, 901(vi00)
--exit
--endexit
