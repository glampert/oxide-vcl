# Preprocessor cases

The test suite of [vclpp](https://github.com/glampert/vclpp) 2 (at 95911cd, MIT
licensed, by the author of this project), run against oxide-vcl's built-in
preprocessor by `crates/oxide-vcl/tests/preprocessor.rs`, plus the port's own
cases where it departs from vclpp: `features/c_expressions`,
`masp/c_expressions` and `errors/question_without_colon`, for C's `?:`, `&&`
and `||`. Each case directory
holds:

- `input.vcl`, and whatever files it includes;
- `args`: oxide-vcl's options for the case, on one line (`-G` or `-g`, and the
  rest), translated from vclpp's;
- `expected.vsm`: when present, the run must succeed and print exactly this;
  when absent, it must fail and print nothing;
- `expected.stderr`: the diagnostics, when there are any.

The outputs are vclpp's, but for `--vcl-boilerplate`'s `.init_vi_all`
(vclpp's `-j` lists VI02 to VI14). The diagnostics are vclpp's, in
oxide-vcl's layout. See [docs/TESTING.md](../../docs/TESTING.md#preprocessor).
