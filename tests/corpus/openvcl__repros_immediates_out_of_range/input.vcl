; oxide-vcl corpus program openvcl__repros_immediates_out_of_range
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/immediates_out_of_range.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_immediates_out_of_range
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    ilw.x iA, 0(vi00)
    iaddi iB, iA, -18
    iaddiu iC, iA, 40000
    isubiu iD, iA, -1
    isw.x iB, 2000(vi00)
    isw.x iC, 3(vi00)
    isw.x iD, 4(vi00)
--exit
--endexit
