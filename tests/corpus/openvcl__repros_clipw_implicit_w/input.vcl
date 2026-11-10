; oxide-vcl corpus program openvcl__repros_clipw_implicit_w
; From https://github.com/glampert/openvcl (test/local-corpus branch) at 5729d1f: repros/clipw_implicit_w.vcl
; Produced: copied unchanged
; License: AFL-2.0, see tests/corpus/licenses/AFL-2.0.txt and tests/corpus/README.md
; The corpus tests drop this header, through the next line, before compiling.
; -- end of corpus header --
.name Repro_clipw_implicit_w
.init_vi_all
.init_vf_all
.syntax new
.vu
--enter
--endenter
    lq fA, 1(vi00)
    clipw.xyz fA, fA
    fcand vi01, 0x3f
    isw.x vi01, 2(vi00)
--exit
--endexit
