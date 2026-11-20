//! Calls of code outside the program (oxide mode's `--call` and `--return`,
//! `docs/VCL_REFERENCE.md`). A caller and a library are compiled apart and
//! laid out in one micro memory, the library at a fixed address, then run
//! together on the VU model. vcl 1.4beta7 has neither annotation.

#[path = "support/vu_model.rs"]
mod vu_model;

use vcl::diagnostics::{CompileMessage, Message, ParseMessage};
use vcl_cli as cli;
use vu_model::{Config, Program, State};

/// Where the library sits in micro memory, in instruction pairs.
const LIBRARY: usize = 200;

/// A library entry: `y = x + 10` in VI11 and `g = (f + f)² + f` in VF24's
/// x, from VI11 and VF24.x. It leaves junk in the rest of its bank (VF25,
/// VF26 and VI12), so a caller value kept there shows.
const ENTRY: &str = ".syntax new\n\
                     .init_vf vf24-vf26\n\
                     .init_vi vi11-vi12\n\
                     --enter\n\
                     in_vi   x(vi11)\n\
                     in_vf   f(vf24)\n\
                     in_vi   vi15\n\
                     --endenter\n\
                     iaddiu  y, x, 10\n\
                     add.x   g, f, f\n\
                     mul.x   h, g, g\n\
                     add.x   g, h, f\n\
                     sub     vf25, vf00, vf00[w]\n\
                     sub     vf26, vf00, vf00[w]\n\
                     iaddiu  vi12, vi00, 999\n\
                     --return vi15\n\
                     out_vi  y(vi11)\n\
                     out_vf.x g(vf24)\n\
                     out_vf  vf25\n\
                     out_vf  vf26\n\
                     out_vi  vi12\n\
                     --endexit\n";

/// The `--call` block for [`ENTRY`].
const CALL: &str = "--call\n\
                    in_vi   vi11\n\
                    in_vf.x vf24\n\
                    out_vi  vi11\n\
                    out_vi  vi12\n\
                    out_vf  vf24\n\
                    out_vf  vf25\n\
                    out_vf  vf26\n\
                    out_vi  vi15\n\
                    --endcall\n";

fn compile(source: &str, unique: &str) -> Result<vcl::Compiled, Vec<vcl::CompileError>> {
    let flag = format!("-u{unique}");
    let options = cli::parse(["-t1", flag.as_str(), "--mode=oxide"])
        .unwrap()
        .compile;
    vcl::compile(source, "/work/call.vcl", &options)
}

fn listing(source: &str, unique: &str) -> String {
    compile(source, unique)
        .unwrap_or_else(|errors| panic!("{errors:?}\n{source}"))
        .assembly
}

/// The caller at address 0 and the library at [`LIBRARY`], with NOP pairs
/// between.
fn micro_memory(caller: &str, library: &str) -> Program {
    let pairs = Program::parse(caller).unwrap().pair_count();
    assert!(pairs <= LIBRARY, "the caller takes {pairs} pairs");
    let padding = format!("{:9}{:<59}NOP\n", "", "NOP").repeat(LIBRARY - pairs);
    let text = format!("{caller}\n{padding}{library}");
    let program = Program::parse(&text).unwrap();
    assert_eq!(
        program.pair_count(),
        LIBRARY + Program::parse(library).unwrap().pair_count()
    );
    program
}

fn run(program: &Program, setup: impl FnOnce(&mut State)) -> State {
    let mut state = State::default();
    setup(&mut state);
    program
        .run(state, Config::TIMED, 10_000)
        .unwrap_or_else(|failure| panic!("{failure}"))
        .state
}

fn vector(state: &State, register: usize) -> [f32; 4] {
    state.vf[register].map(f32::from_bits)
}

/// What [`ENTRY`] makes of `f` in its x lane.
fn entry_value(f: f32) -> f32 {
    (f + f) * (f + f) + f
}

/// A caller whose allowed registers are the library's bank and, numbered
/// above it, just enough for what is live across the call: three vectors in
/// VF27-VF29 and two integers in VI13-VI14. An allocator that took the
/// lower bank registers would lose them to the call.
fn tight_caller(extra: &str) -> String {
    format!(
        ".syntax new\n\
         .init_vf vf24-vf29\n\
         .init_vi vi01\n\
         .init_vi vi11-vi14\n\
         {extra}\
         --enter\n\
         in_vf   a(vf01)\n\
         in_vi   n(vi01)\n\
         --endenter\n\
         add.x   vf24, vf00, a[y]\n\
         add     b, a, a\n\
         mul     c, a, a\n\
         sub     e, a, vf00[w]\n\
         iaddiu  m, n, 3\n\
         iaddiu  mm, n, 7\n\
         iaddiu  vi11, n, 5\n\
         iaddiu  address, vi00, {LIBRARY}\n\
         {CALL}\
         jalr    vi15, address\n\
         iaddiu  result, vi11, 0\n\
         add     d, b, c\n\
         add     d, d, e\n\
         add.x   d, d, vf24[x]\n\
         iadd    k, m, result\n\
         iadd    k, k, mm\n\
         --exit\n\
         out_vf  d(vf02)\n\
         out_vi  k(vi02)\n\
         --endexit\n"
    )
}

fn check_tight(state: &State, a: [f32; 4], n: u16) {
    let d: [f32; 4] = std::array::from_fn(|lane| {
        let sum = (a[lane] + a[lane]) + a[lane] * a[lane] + (a[lane] - 1.0);
        if lane == 0 {
            sum + entry_value(a[1])
        } else {
            sum
        }
    });
    assert_eq!(vector(state, 2), d);
    assert_eq!(state.vi[2], (n + 3) + (n + 5 + 10) + (n + 7));
}

#[test]
fn values_live_across_a_call_stay_out_of_its_registers() {
    let caller = listing(&tight_caller(""), "CALLER");
    // The call is a JALR through the address register, with VI15 linked.
    assert!(caller.contains("jalr          VI15,"), "{caller}");
    let library = listing(ENTRY, "ENTRY");
    assert!(library.contains("jr            VI15"), "{library}");
    assert!(!library.contains("[E]"), "{library}");
    let program = micro_memory(&caller, &library);
    let a = [1.5, 2.0, -0.5, 4.0];
    let state = run(&program, |state| {
        state.set_vector(1, a);
        state.vi[1] = 6;
    });
    check_tight(&state, a, 6);
}

#[test]
fn values_spill_when_a_call_leaves_too_few_registers() {
    // VF29 is not allowed: one of the three vectors live across the call
    // goes to the work area.
    let source = tight_caller(".vu_work_area vu1, 900, 4\n")
        .replace(".init_vf vf24-vf29\n", ".init_vf vf24-vf28\n");
    let caller = listing(&source, "CALLER");
    assert!(caller.contains("900(VI00)"), "{caller}");
    let program = micro_memory(&caller, &listing(ENTRY, "ENTRY"));
    let a = [3.0, -1.0, 0.25, 2.0];
    let state = run(&program, |state| {
        state.set_vector(1, a);
        state.vi[1] = 40;
    });
    check_tight(&state, a, 40);
}

#[test]
fn calls_on_two_paths_join() {
    let source = format!(
        ".syntax new\n\
         .init_vf_all\n\
         .init_vi_all\n\
         --enter\n\
         in_vf   a(vf01)\n\
         in_vi   n(vi01)\n\
         --endenter\n\
         add     b, a, a\n\
         iaddiu  address, vi00, {LIBRARY}\n\
         ibeq    n, vi00, other\n\
         iaddiu  vi11, n, 1\n\
         add.x   vf24, vf00, a[x]\n\
         {CALL}\
         jalr    vi15, address\n\
         b       joined\n\
         other:\n\
         iaddiu  vi11, vi00, 100\n\
         add.x   vf24, vf00, a[z]\n\
         {CALL}\
         jalr    vi15, address\n\
         joined:\n\
         iaddiu  k, vi11, 0\n\
         add.x   b, b, vf24[x]\n\
         --exit\n\
         out_vf  b(vf02)\n\
         out_vi  k(vi02)\n\
         --endexit\n"
    );
    let caller = listing(&source, "CALLER");
    assert_eq!(caller.matches("jalr").count(), 2, "{caller}");
    let program = micro_memory(&caller, &listing(ENTRY, "ENTRY"));
    let a = [0.5, 1.0, 2.0, 3.0];
    for n in [0, 4] {
        let state = run(&program, |state| {
            state.set_vector(1, a);
            state.vi[1] = n;
        });
        let (argument, f) = if n == 0 { (100, a[2]) } else { (n + 1, a[0]) };
        let mut b = a.map(|v| v + v);
        b[0] += entry_value(f);
        assert_eq!(vector(&state, 2), b, "n = {n}");
        assert_eq!(state.vi[2], argument + 10, "n = {n}");
    }
}

#[test]
fn the_address_may_be_in_a_register_the_call_writes() {
    // Few values are live, so the address can take VI12, which the call
    // writes after the JALR has read it.
    let source = format!(
        ".syntax new\n\
         .init_vf_all\n\
         .init_vi vi11-vi12\n\
         --enter\n\
         in_vf   a(vf01)\n\
         in_vi   n(vi01)\n\
         --endenter\n\
         add     b, a, a\n\
         iaddiu  vi11, n, 5\n\
         add.x   vf24, vf00, a[x]\n\
         iaddiu  address, vi00, {LIBRARY}\n\
         {CALL}\
         jalr    vi15, address\n\
         iaddiu  result, vi11, 0\n\
         mul     c, b, a\n\
         --exit\n\
         out_vf  c(vf02)\n\
         out_vi  result(vi02)\n\
         --endexit\n"
    );
    let caller = listing(&source, "CALLER");
    assert!(caller.contains("jalr          VI15,VI12"), "{caller}");
    let program = micro_memory(&caller, &listing(ENTRY, "ENTRY"));
    let a = [1.0, 2.0, 3.0, 4.0];
    let state = run(&program, |state| {
        state.set_vector(1, a);
        state.vi[1] = 9;
    });
    assert_eq!(vector(&state, 2), a.map(|v| (v + v) * v));
    assert_eq!(state.vi[2], 9 + 5 + 10);
}

#[test]
fn a_call_block_needs_its_jalr_and_physical_registers() {
    let call = |declarations: &str, after: &str| {
        format!(
            ".syntax new\n\
             .init_vf_all\n\
             .init_vi_all\n\
             --enter\n\
             in_vi   n(vi01)\n\
             --endenter\n\
             iaddiu  address, n, 1\n\
             --call\n\
             {declarations}\
             --endcall\n\
             {after}\
             --exit\n\
             --endexit\n"
        )
    };
    let message = |source: &str| {
        let errors = compile(source, "X").expect_err("rejected");
        errors[0].message.clone()
    };
    assert_eq!(
        message(&call("in_vi vi11\n", "iaddiu t, address, 1\n")),
        Message::Compile(CompileMessage::CallWithoutJalr)
    );
    assert_eq!(
        message(&call("in_vi count\n", "jalr vi15, address\n")),
        Message::Compile(CompileMessage::CallRegisterNotPhysical {
            name: "count".into()
        })
    );
    // vcl 1.4beta7 knows no --call.
    let options = cli::parse(["-t1", "-uX"]).unwrap().compile;
    let errors = vcl::compile(
        &call("in_vi vi11\n", "jalr vi15, address\n"),
        "/work/call.vcl",
        &options,
    )
    .expect_err("rejected in vcl mode");
    assert!(
        matches!(
            &errors[0].message,
            Message::Parse(ParseMessage::UnknownInstruction { opcode }) if opcode == "--call"
        ),
        "{errors:?}"
    );
}

#[test]
fn a_return_counts_its_rows() {
    // The JR and its delay slot are part of the program's size.
    let library = compile(ENTRY, "ENTRY").unwrap();
    let pairs = Program::parse(&library.assembly).unwrap().pair_count();
    assert_eq!(library.instruction_pairs, pairs, "{}", library.assembly);
    assert!(
        library.assembly.contains(&format!(";\tiCount={pairs}\n")),
        "{}",
        library.assembly
    );
}

#[test]
fn a_return_needs_its_register() {
    let source = ".syntax new\n\
                  .init_vi_all\n\
                  --enter\n\
                  in_vi   x(vi11)\n\
                  --endenter\n\
                  iaddiu  y, x, 1\n\
                  --return vf02\n\
                  out_vi  y(vi11)\n\
                  --endexit\n";
    let errors = compile(source, "X").expect_err("rejected");
    assert_eq!(
        errors[0].message,
        Message::Compile(CompileMessage::ReturnRegister)
    );
}
