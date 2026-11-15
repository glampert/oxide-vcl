# Preprocessor

`-G` and `-g` run the preprocessor in `crates/vcl/src/preprocessor` before the
parser, in the same run. Its behaviour is
[vclpp](https://github.com/glampert/vclpp) 2's (at 95911cd), the preprocessor
whose output the corpus's doom3, quake2, tyra and ps2gl inputs are. The user
guide is [VCL_REFERENCE.md, Preprocessor](VCL_REFERENCE.md#preprocessor). The
compiler is not involved: it receives the preprocessed text, and the driver
maps its diagnostics back to the lines the text came from.

vcl 1.4beta7 instead runs external programs for these options (`gcc -P -E -C
-x assembler-with-cpp` for `-G`, `gasp -p -s -c ';'` for `-g`; see
[OUTPUT_FORMAT.md](OUTPUT_FORMAT.md#preprocessing)). Its code generation does
not depend on which preprocessor made the text, so both modes use the
built-in one.

## Layout

| File | Contents |
| --- | --- |
| `mod.rs` | `preprocess(name, text, options, loader)`, `PreprocessOptions`, the result types |
| `lexer.rs` | Tokens, kept as written with the whitespace before each |
| `expression.rs` | Integer constant expressions (`#if`, `.aif`, `--fold-constants`) |
| `source.rs` | The file table, include search, `#pragma once`, `SourceLoader` |
| `macros.rs` | The macro table and the built-ins |
| `engine.rs` | Lines, files, C directives, conditionals and the diagnostics' notes |
| `expander.rs` | Macro expansion |
| `masp.rs` | MASP mode's first stage |
| `output.rs` | The output text, `--vcl-boilerplate`, `--fold-constants`, `--flatten-subscripts`, line origins |

Everything but `lexer.rs` and `expression.rs` follows vclpp's own sources.
Those two follow vclpp's behaviour, observed by running it, and were written
for this project.

`SourceLoader` reads included files: `FileSystem` in the driver, any other
implementation in tests. An error stops the run (`Fallible`, a boxed
`Diagnostic`); warnings are collected and printed by the driver before the
error or the output. Every message is a `PreprocessMessage` with vclpp's
wording, printed in the `Reporter`'s layout: vclpp's `file:line:col` becomes
`file(line)` with a caret at the column, its include chain and expansion
chain become notes after the message, and `[-Wundef]` reads
`[--warn-undef]`.

## Tokens

Observed from vclpp 2, one token at a time:

- **Comments.** `;`, `//` and `/* */`. A comment between two tokens on a line
  counts as one space; before the first token of a line it turns into spaces,
  so the token keeps its column. A `/* */` comment that spans lines does not
  end the line it is in, and a `/*` inside one is a warning.
- **Strings and character literals.** Kept as written. A backslash is an
  ordinary character, so `"a\"b"` is the string `"a\"` followed by an
  unterminated one. A character literal holds at most one character: `'\n'`
  is an error.
- **Numbers.**
  - `0x` is followed by hex digits and `0b` by binary digits, each with none
    at all; both take an integer suffix.
  - A `0` followed by anything but `.` is octal: the `0` and the decimal
    digits after it. So `0e5` is `0` then `e5`, and `00.5` is `00` then `.5`.
  - Otherwise a number is digits with at most one dot (it may start with
    one), then an optional exponent: `e`, an optional sign and digits, which
    may be absent (`1e`). A dot or an exponent makes it a float.
  - Suffixes: a float takes one `f` or `l`, an integer any run of `u` and
    `l`, in either case. So `12f` is `12` then `f`, and `1.5ff` is `1.5f`
    then `f`.
  - A second dot before the exponent is an error (`1..5`, `.5.5`), and so is
    a `#` right after a float's fraction (`1.#INF`, `5.#`). A dot after the
    exponent starts a new number (`1.5e5.5` is `1.5e5` then `.5`).
- **Punctuation.** The longest of the C operators and `#`, `##`, `$`, `\` and
  `...`. Any other character, such as `@` or a backtick, is a token of its
  own.
- **Splices.** A `\` that ends a line joins the next line to it.

## Integer expressions

`#if`, masp's `.aif`, `.awhile`, `.assigna`, `.equ` and `.arepeat`, and
`--fold-constants` share one evaluator (`expression.rs`). Its operators work
as in C:

- **Operators.** C's, at C's precedence. Arithmetic wraps in 64 bits, and so
  do literals too large for it. A shift amount counts modulo 64 (`1 << 64` is
  1), and division truncates toward zero.
- **`?:`.** C's conditional operator, below `||`, grouping to the right:
  `1 ? 0 : 1 ? 1 : 1` is 0. Either branch can hold another `?:`, with or
  without parentheses, and a `?` without its `:` is an error.
- **Unevaluated operands.** The right side of `&&` when the left is 0, of
  `||` when it is not, and the branch of `?:` not taken are checked for
  syntax but not evaluated, so `#if defined(N) && 64 / N` is safe when `N` is
  not defined.
- **Octal.** A literal with an 8 or 9 in it is a syntax error.
- **Errors.** Their wording and the order they are found in are vclpp's:
  - `invalid operator 'X' after operator`: a binary operator where a value
    belongs;
  - `invalid logic not or two's complement after value`: `!` or `~` after a
    value;
  - `invalid operator 'X'`: an operator that has no place here (`=`, `++`,
    `,`);
  - `syntax error`: two values in a row;
  - `too many ')'`: a `)` with no open `(`;
  - `trailing operator`: one left at the end, or before `)`;
  - `too many '('`: a `(` that is never closed;
  - `'?' without ':'` and `':' without '?'`.

`--fold-constants` folds only expressions that stand on their own and apply a
binary operator, using the same evaluator.

## Output and line origins

Each line of output keeps the first token's indentation and the spacing
between its tokens; spacing inside a one-line macro expansion is one space
or none. A block macro expansion written in the source is set off by blank
lines, as an MASP expansion is.

`Preprocessed::line_origins` gives, for each output line, the line of the
first token written on it (one that did not come out of a macro). For a line
that is all a block macro's body, it is the body's line, in the file that
defines the macro. The driver hands these to `Reporter::set_line_origins`, so
a parse or compile error names that file and line and quotes the line from
there (`driver_output.rs`,
`compile_errors_in_preprocessed_code_name_the_line_it_came_from`).

## Differences from vclpp

- `--vcl-boilerplate` declares `.init_vi_all`; vclpp's `-j` lists VI02 to
  VI14, because openvcl gives VI01 to variables that FCAND, FCEQ and FCOR
  overwrite. vcl 1.4beta7 keeps VI01 free across them (observed, probes
  `vi01-n14-fc`, `vi01-n15-fc` and `vi01-n14-branch`; and VI01 used when
  there is no FCAND, `vi01-n15-nofc`), and so does the port
  (`flag_registers.rs`).
- vclpp's options have long names here, because VCL uses `-j`, `-x`, `-f`,
  `-D`, `-c` and `-m` for other things (table in
  [VCL_REFERENCE.md](VCL_REFERENCE.md#preprocessor-options)).
  `--warnings-as-errors` is vclpp's `-Werror`.
- Messages are printed in the `Reporter`'s layout, and a refused GASP
  directive is "not supported by the preprocessor".
- `-g+` (gasp's alternate syntax) is an error when preprocessing runs.
- Integer expressions follow C where vclpp does not (observed from vclpp 2,
  `#if` lines run one at a time):
  - vclpp reads `?` and `:` as two operators of the lowest precedence that
    group to the left and share one pending condition. So
    `1 ? 0 : 1 ? 1 : 1` is 1 there, `1 ? 2 : (3 ? 4 : 5)` and
    `2 ? 3 ? 4 : 5 : 6` are errors ("'?' after '?'"), and `a ? b` without a
    `:` is `b`.
  - vclpp evaluates every operand, so `0 && 1 / 0` is a division by zero
    there.

## Tests

- **`tests/preprocessor/<group>/<case>/`**: vclpp's own test suite, and the
  port's cases where it departs from vclpp (`features/c_expressions`,
  `masp/c_expressions`, `errors/question_without_colon`), each with its
  options in `args` and the expected output and diagnostics, run by
  `crates/oxide-vcl/tests/preprocessor.rs`. `VCL_UPDATE_PREPROCESSOR=1`
  rewrites the expected files.
- **Corpus round trip.** `tests/corpus/sources/<project>` holds the sources
  of the 22 corpus inputs vclpp produced, at the commits their headers name.
  `vclpp_made_corpus_inputs_come_out_the_same` preprocesses each with the
  flags its header records and compares the result with the input, line for
  line, but for the boilerplate's `.init_vi` line.
- **Units.** Unit tests in `lexer.rs`, `expression.rs` and `source.rs`; the
  options in `cli.rs`; the driver's behaviour in `driver_output.rs`.

openvcl's `examples/triangle-gasp` source (`tests/corpus/openvcl__examples_triangle-gasp_vu1Triangle/source.vcl`)
uses `.assign`, which vclpp refuses and so does the preprocessor; its corpus
input was made by masp.
