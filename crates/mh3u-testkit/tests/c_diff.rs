//! The translator's C against the reference interpreter. Every instruction word of the instruction set is translated to a C function,
//! all of them are compiled into one program with the system C compiler, and the program is run on the same random register states
//! the interpreter was run on; what they leave behind must be identical. (The interpreter is itself checked against QEMU in
//! `qemu_diff.rs`, so this chain ends at an independent implementation.)

use mh3u_ppc::Op::{self, *};
use mh3u_ppc::{Insn, interp::Stop};
use mh3u_recomp::{RUNTIME_H, emit_function, function_name};
use mh3u_testkit::harness::*;
use mh3u_testkit::*;
use std::collections::BTreeSet;
use std::io::Write;
use std::process::{Command, Stdio};

/// A C compiler, if there is one.
fn compiler() -> Option<&'static str> {
    ["clang", "cc", "gcc"].into_iter().find(|c| {
        Command::new(c)
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|s| s.success())
    })
}

const MAIN: &str = r#"
#include "ppc.h"
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#define STATE 0x400
#define IN_BASE 0x10002000u
#define SCRATCH_AT 0x200
#define SCRATCH_LEN 256
/* the hooks look at the registers and change them, as the runtime's do: the tests of calls check that the translated code wrote its
   registers back before a hook and read them again after it (no other test reaches a hook) */
void rt_syscall(Ctx *c) { c->r[27] = c->r[3]; c->r[3] ^= 0xffffu; }
void rt_trap(Ctx *c, uint32_t w) { (void)c; (void)w; }
void rt_unsupported(Ctx *c, uint32_t w) { (void)c; (void)w; }
void rt_call(Ctx *c, uint32_t t) { c->r[29] = c->r[3]; c->r[3] += 0x100; c->r[4] = t; }
void rt_jump(Ctx *c, uint32_t t) { c->r[28] = c->r[3]; c->r[4] = t; }
typedef void (*Fn)(Ctx *);
extern Fn table[];
static uint32_t be32(const uint8_t *p) { return (uint32_t)p[0] << 24 | p[1] << 16 | p[2] << 8 | p[3]; }
static uint64_t be64(const uint8_t *p) { return (uint64_t)be32(p) << 32 | be32(p + 4); }
static void put32(uint8_t *p, uint32_t v) { p[0] = v >> 24; p[1] = v >> 16; p[2] = v >> 8; p[3] = v; }
static void put64(uint8_t *p, uint64_t v) { put32(p, v >> 32); put32(p + 4, (uint32_t)v); }
int main(int argc, char **argv) {
    int idx = atoi(argv[1]);
    size_t n = (size_t)atoi(argv[2]);
    uint8_t *mem = mmap((void *)0x10000000, 0x800000, PROT_READ | PROT_WRITE, MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0);
    if (mem == MAP_FAILED) return 2;
    uint8_t *in = malloc(n * STATE), *out = calloc(n, STATE);
    if (fread(in, 1, n * STATE, stdin) != n * STATE) return 3;
    for (size_t i = 0; i < n; i++) {
        const uint8_t *s = in + i * STATE;
        Ctx c;
        memset(&c, 0, sizeof c);
        for (int k = 0; k < 32; k++) { c.r[k] = be32(s + 4 * k); c.f[k].u = be64(s + 0x80 + 8 * k); c.ps1[k].u = be64(s + 0x300 + 8 * k); }
        for (int k = 0; k < 8; k++) c.gqr[k] = be32(s + 0x1a0 + 4 * k);
        c.cr = be32(s + 0x180); c.xer = be32(s + 0x184); c.lr = be32(s + 0x188); c.ctr = be32(s + 0x18c);
        c.r[30] = 0; c.r[31] = IN_BASE + (uint32_t)(i * STATE);
        uint32_t sa = IN_BASE + (uint32_t)(i * STATE) + SCRATCH_AT;
        memcpy((void *)(uintptr_t)sa, s + SCRATCH_AT, SCRATCH_LEN);
        c.mem = 0;
        table[idx](&c);
        uint8_t *o = out + i * STATE;
        for (int k = 0; k < 32; k++) { put32(o + 4 * k, c.r[k]); put64(o + 0x80 + 8 * k, c.f[k].u); put64(o + 0x300 + 8 * k, c.ps1[k].u); }
        for (int k = 0; k < 8; k++) put32(o + 0x1a0 + 4 * k, c.gqr[k]);
        put32(o + 0x180, c.cr); put32(o + 0x184, c.xer); put32(o + 0x188, c.lr); put32(o + 0x18c, c.ctr);
        memcpy(o + SCRATCH_AT, (void *)(uintptr_t)sa, SCRATCH_LEN);
    }
    fwrite(out, 1, n * STATE, stdout);
    return 0;
}
"#;

/// Compile the translation of each `programs[i]` (a list of words at the test address) into a program with a table of them.
fn build_program(programs: &[Vec<u32>], tag: &str) -> Result<std::path::PathBuf, String> {
    let start = test_address();
    let none = BTreeSet::new();
    let mut src = String::new();
    for (i, words) in programs.iter().enumerate() {
        let c = emit_function(start, words, &none).replace(&function_name(start), &format!("t_{i}"));
        src.push_str(&c);
    }
    let names: Vec<String> = (0..programs.len()).map(|i| format!("t_{i}")).collect();
    compile(&src, &names, tag)
}

/// Compile translated functions (`src`) into a program whose table holds the functions `entries`.
fn compile(src: &str, entries: &[String], tag: &str) -> Result<std::path::PathBuf, String> {
    let cc = compiler().ok_or("no C compiler")?;
    let dir = std::env::temp_dir().join(format!("mh3u-c-diff-{}-{tag}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("ppc.h"), RUNTIME_H).map_err(|e| e.to_string())?;
    let mut src = format!("#include \"ppc.h\"\n{src}");
    src.push_str("typedef void (*Fn)(Ctx *);\nFn table[] = {");
    for name in entries {
        src.push_str(&format!("{name},"));
    }
    src.push_str("};\n");
    std::fs::write(dir.join("gen.c"), src).map_err(|e| e.to_string())?;
    std::fs::write(dir.join("main.c"), MAIN).map_err(|e| e.to_string())?;
    let exe = dir.join("prog");
    let out = Command::new(cc)
        .args(["-O1", "-w", "-fno-strict-aliasing", "-I"])
        .arg(&dir)
        .arg(dir.join("gen.c"))
        .arg(dir.join("main.c"))
        .arg("-o")
        .arg(&exe)
        .arg("-lm")
        .output()
        .map_err(|e| e.to_string())?;
    if !out.status.success() {
        return Err(format!(
            "the generated C did not compile:\n{}",
            String::from_utf8_lossy(&out.stderr).chars().take(3000).collect::<String>()
        ));
    }
    Ok(exe)
}

fn run_program(exe: &std::path::Path, idx: usize, states: &[St]) -> Result<Vec<St>, String> {
    let mut input = Vec::new();
    for s in states {
        input.extend(state_bytes(s));
    }
    let mut child = Command::new(exe)
        .arg(idx.to_string())
        .arg(states.len().to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    child.stdin.take().expect("stdin").write_all(&input).map_err(|e| e.to_string())?;
    let out = child.wait_with_output().map_err(|e| e.to_string())?;
    if !out.status.success() || out.stdout.len() != states.len() * STATE {
        return Err(format!("the program stopped: {:?}", out.status));
    }
    Ok(out.stdout.chunks(STATE).map(state_from_bytes).collect())
}

/// Not run: what the translator does not give the interpreter's meaning to (it is a call or a hook), and what QEMU-checked tests also skip
/// for good reasons.
fn skipped_here(op: Op) -> Option<&'static str> {
    Some(match op {
        Only64 | Eciwx | Ecowx => "not on this processor",
        Mfmsr | Mtmsr | Mfsr | Mtsr | Mfsrin | Mtsrin | Rfi | Tlbie | Tlbia | Tlbsync | Dcbi => "privileged",
        Sc | Tw | Twi => "a hook, not a state change",
        DcbzL => "paired-single mode",
        B | Bc | Bclr | Bcctr => "branches have their own test",
        Lwarx | Stwcx => "the reservation pair has its own test",
        _ => return None,
    })
}

#[test]
fn the_translated_c_of_every_instruction_does_what_the_interpreter_does() {
    if compiler().is_none() {
        eprintln!("skipped: no C compiler");
        return;
    }
    let mut rng = Rng(0x7a6b_5c4d_3e2f_1a09);
    // one word per variant of every operation, with the states for it
    struct Item {
        op: Op,
        word: u32,
        states: Vec<St>,
    }
    let mut items: Vec<Item> = Vec::new();
    for &op in Op::ALL {
        if skipped_here(op).is_some() {
            continue;
        }
        let tpl = template(op);
        for _ in 0..VARIANTS {
            let word = build(op, tpl, &mut rng, 0).word[0];
            let mut states = Vec::new();
            for i in 0..CASES {
                let mut c = build(op, tpl, &mut rng, i);
                fix_state_for_word(op, word, &mut c.st, i, &mut rng);
                // the harness registers
                states.push(c.st);
            }
            items.push(Item { op, word, states });
        }
    }
    let programs: Vec<Vec<u32>> = items.iter().map(|it| vec![it.word]).collect();
    let exe = build_program(&programs, "ops").expect("the translated C builds");
    let mut failures = Vec::new();
    for (i, it) in items.iter().enumerate() {
        let c_states = match run_program(&exe, i, &it.states) {
            Ok(s) => s,
            Err(e) => {
                failures.push(format!("{} {:#010x}: {e}", it.op.name(), it.word));
                continue;
            }
        };
        for (k, st) in it.states.iter().enumerate() {
            match interpret(&[it.word], st, k) {
                Ok((ours, _)) => {
                    let mut d = differences(it.op, &c_states[k], &ours);
                    d.extend(ps_differences(&c_states[k], &ours));
                    if !d.is_empty() {
                        failures.push(format!(
                            "{} {:#010x} case {k}: {} (c first, interpreter second)",
                            it.op.name(),
                            it.word,
                            d.join("; ")
                        ));
                        break;
                    }
                }
                Err(Stop::Unsupported(w)) => {
                    failures.push(format!("{} {w:#010x}: the interpreter does not model it", it.op.name()));
                    break;
                }
                Err(e) => {
                    failures.push(format!("{} {:#010x}: interpreter {e:?}", it.op.name(), it.word));
                    break;
                }
            }
        }
    }
    eprintln!("{} words translated, compiled and compared", items.len());
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.iter().take(60).cloned().collect::<Vec<_>>().join("\n")
    );
    // a word of every kind decodes (guards the test itself)
    assert!(Insn::decode(items[0].word).is_some());
}

/// Branches in the translated C: forward jumps inside the function, and a return (`bclr`) to a place outside it. (A call, or a jump through
/// a register, is a hook in the C, not a change of where the program counter is, so those are not compared with the interpreter.)
#[test]
fn the_translated_branches_go_where_the_interpreter_goes() {
    if compiler().is_none() {
        return;
    }
    let mut rng = Rng(0x0bad_cafe_1234_5678);
    let valid_bo: [u32; 17] = [0, 1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20];
    let marker = |rd: u32| 0x3800_0001 | rd << 21 | rd << 16;
    let mut programs: Vec<Vec<u32>> = Vec::new();
    let mut kinds: Vec<&str> = Vec::new();
    for variant in 0..60 {
        let (branch, kind) = match variant % 3 {
            0 => (0x4800_0008u32, "b"),
            1 => (0x4000_0000 | valid_bo[rng.below(17) as usize] << 21 | rng.below(32) << 16 | 8, "bc"),
            _ => (0x4c00_0020 | valid_bo[rng.below(17) as usize] << 21 | rng.below(32) << 16, "bclr"),
        };
        programs.push(vec![branch, marker(3), marker(4)]);
        kinds.push(kind);
    }
    let exe = build_program(&programs, "branches").expect("the translated C builds");
    let mut failures = Vec::new();
    for (i, words) in programs.iter().enumerate() {
        let mut states = Vec::new();
        for _ in 0..CASES {
            let mut st = St::default();
            for r in 0..30 {
                st.gpr[r] = rng.word();
            }
            st.cr = rng.next_u64() as u32;
            st.xer = (rng.next_u64() as u32) & 0xe000_0000;
            st.ctr = rng.below(4);
            // a return goes to an address outside the function, which both then treat as the end
            st.lr = 0x4;
            states.push(st);
        }
        let c_states = run_program(&exe, i, &states).expect("the program ran");
        for (k, st) in states.iter().enumerate() {
            let (ours, _) = interpret(words, st, k).expect("the interpreter ran");
            let d = differences(Add, &c_states[k], &ours);
            if !d.is_empty() {
                failures.push(format!("{} {words:08x?} case {k}: {} (c first)", kinds[i], d.join("; ")));
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// A switch: a table of branches after the `bctr`, as the game's compiler makes them. The translated C picks the branch by the index;
/// the interpreter runs the jump through the register.
#[test]
fn the_translated_switch_goes_where_the_interpreter_goes() {
    if compiler().is_none() {
        return;
    }
    let base = test_address() + 4 * 7;
    let lo = base as u16 as i16;
    let hi = (base.wrapping_sub(lo as i32 as u32) >> 16) as u16;
    // case k sets r6 to k + 1 and goes to the end, which sets r7
    let mut words = vec![
        0x2803_0003,                        // cmplwi r3, 3
        0x4181_0000 | (4 * 18),             // bgt end
        0x5464_103a,                        // slwi r4, r3, 2
        0x3ca4_0000 | u32::from(hi),        // addis r5, r4, hi
        0x38a5_0000 | u32::from(lo as u16), // addi r5, r5, lo
        0x7ca9_03a6,                        // mtctr r5
        0x4e80_0420,                        // bctr
    ];
    for k in 0..4u32 {
        words.push(0x4800_0000 | (4 * (4 + k))); // b case k: entry k is at 7 + k, case k at 11 + 2k
    }
    for k in 0..4u32 {
        words.push(0x38c0_0000 | (k + 1)); // li r6, k + 1
        words.push(0x4800_0000 | (4 * (7 - 2 * k))); // b end
    }
    words.push(0x38e7_0001); // end: addi r7, r7, 1
    let exe = build_program(&[words.clone()], "switch").expect("the translated C builds");
    let mut rng = Rng(0x5eed_0000_1111_2222);
    let mut states = Vec::new();
    for _ in 0..CASES {
        let mut st = St::default();
        for r in 0..30 {
            st.gpr[r] = rng.word();
        }
        st.gpr[3] = rng.below(6);
        st.lr = 0x4;
        states.push(st);
    }
    let c_states = run_program(&exe, 0, &states).expect("the program ran");
    let mut failures = Vec::new();
    for (k, st) in states.iter().enumerate() {
        let (ours, _) = interpret(&words, st, k).expect("the interpreter ran");
        let mut d = differences(Add, &c_states[k], &ours);
        d.extend(ps_differences(&c_states[k], &ours));
        if !d.is_empty() {
            failures.push(format!("r3 = {}: {}", st.gpr[3], d.join("; ")));
        }
    }
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// `lwarx` and `stwcx.`, the reservation pair.
#[test]
fn the_translated_reservation_pair_matches_the_interpreter() {
    if compiler().is_none() {
        return;
    }
    let mut rng = Rng(0x1357_9bdf_0246_8ace);
    let mut programs: Vec<Vec<u32>> = Vec::new();
    for variant in 0..9 {
        let (rs, ra, rb, rd) = (rng.below(25), 25 + rng.below(2), 28, rng.below(25));
        let lwarx = 0x7c00_0028 | rd << 21 | ra << 16 | rb << 11;
        let stwcx = 0x7c00_012d | rs << 21 | ra << 16 | rb << 11;
        let other = 0x7c00_0028 | rd << 21 | ra << 16 | 27 << 11;
        programs.push(match variant % 3 {
            0 => vec![lwarx, stwcx],
            1 => vec![stwcx],
            _ => vec![other, stwcx],
        });
    }
    let exe = build_program(&programs, "larx").expect("the translated C builds");
    let mut failures = Vec::new();
    for (i, words) in programs.iter().enumerate() {
        let (ra, rb) = (((words[0] >> 16) & 31) as usize, ((words[0] >> 11) & 31) as usize);
        let mut states = Vec::new();
        for k in 0..CASES {
            let mut st = St::default();
            for r in 0..30 {
                st.gpr[r] = rng.word();
            }
            st.gpr[ra] = scratch_addr(k) + 96;
            st.gpr[rb] = 4 * rng.below(8);
            st.gpr[27] = 4 * rng.below(8) + 4;
            st.cr = rng.next_u64() as u32;
            st.xer = (rng.next_u64() as u32) & 0xe000_0000;
            states.push(st);
        }
        let c_states = run_program(&exe, i, &states).expect("the program ran");
        for (k, st) in states.iter().enumerate() {
            let (ours, _) = interpret(words, st, k).expect("the interpreter ran");
            let d = differences(Add, &c_states[k], &ours);
            if !d.is_empty() {
                failures.push(format!("larx {words:08x?} case {k}: {}", d.join("; ")));
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

/// Calls, jumps out and system calls: the translated code keeps registers in locals, so it must write them back before a hook (which
/// reads them, in the stubs above, into r27 to r29) and read them again after it (the stubs change r3 and r4). Checked against what
/// the hooks do, not against the interpreter (for which a call is a jump).
#[test]
fn the_registers_are_written_back_and_read_again_around_hooks() {
    if compiler().is_none() {
        return;
    }
    let at = test_address();
    let programs: Vec<Vec<u32>> = vec![
        vec![0x3863_0001, 0x4800_1001, 0x3863_0001], // addi r3, r3, 1; bl +0x1000 (not a function: rt_call); addi r3, r3, 1
        vec![0x3863_0001, 0x4800_1000],              // addi r3, r3, 1; b +0x1000 (rt_jump)
        vec![0x3863_0001, 0x4400_0002, 0x3863_0001], // addi r3, r3, 1; sc (rt_syscall); addi r3, r3, 1
    ];
    let exe = build_program(&programs, "hooks").expect("the translated C builds");
    let mut rng = Rng(0x600d_f00d_0bad_beef);
    let mut states = Vec::new();
    for _ in 0..CASES {
        let mut st = St::default();
        for r in 0..30 {
            st.gpr[r] = rng.word();
        }
        st.lr = 0x4;
        states.push(st);
    }
    let mut failures = Vec::new();
    for (i, _) in programs.iter().enumerate() {
        let out = run_program(&exe, i, &states).expect("the program ran");
        for (st, got) in states.iter().zip(&out) {
            let r3 = st.gpr[3];
            let mut want = st.clone();
            match i {
                0 => {
                    want.gpr[29] = r3.wrapping_add(1);
                    want.gpr[3] = r3.wrapping_add(0x102);
                    want.gpr[4] = at + 4 + 0x1000;
                    want.lr = at + 8;
                }
                1 => {
                    want.gpr[28] = r3.wrapping_add(1);
                    want.gpr[3] = r3.wrapping_add(1);
                    want.gpr[4] = at + 4 + 0x1000;
                }
                _ => {
                    want.gpr[27] = r3.wrapping_add(1);
                    want.gpr[3] = (r3.wrapping_add(1) ^ 0xffff).wrapping_add(1);
                }
            }
            if got.gpr[..30] != want.gpr[..30] || got.lr != want.lr {
                failures.push(format!(
                    "program {i}: got {:08x?} lr {:#x}, want {:08x?} lr {:#x}",
                    &got.gpr[..30],
                    got.lr,
                    &want.gpr[..30],
                    want.lr
                ));
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

/// Calls and tail calls between translated functions: a tail call is a jump (`TAIL`, `musttail` where the compiler has it), and the
/// registers the caller changed reach the callee, and the callee's reach the caller after a call.
#[test]
fn calls_and_tail_calls_between_functions_carry_the_registers() {
    if compiler().is_none() {
        return;
    }
    let at = test_address();
    let (callee, tail, caller) = (at + 0x100, at, at + 0x200);
    let known = BTreeSet::from([callee, tail, caller]);
    let rel = |from: u32, to: u32| to.wrapping_sub(from) & 0x03ff_fffc;
    // the callee: addi r3, r3, 2; mulli r5, r3, 3; blr
    let callee_words = vec![0x3863_0002, 0x1ca3_0003, 0x4e80_0020];
    // a tail call: addi r3, r3, 1; b callee
    let tail_words = vec![0x3863_0001, 0x4800_0000 | rel(tail + 4, callee)];
    // a call: addi r3, r3, 1; bl callee; add r6, r5, r3; blr
    let caller_words = vec![0x3863_0001, 0x4800_0001 | rel(caller + 4, callee), 0x7cc5_1a14, 0x4e80_0020];
    let mut src = String::new();
    for start in [callee, tail, caller] {
        src.push_str(&format!("void {}(Ctx *c);\n", function_name(start)));
    }
    for (start, words) in [(callee, &callee_words), (tail, &tail_words), (caller, &caller_words)] {
        src.push_str(&mh3u_recomp::emit_function(start, words, &known));
    }
    let exe = compile(&src, &[function_name(tail), function_name(caller)], "calls").expect("the translated C builds");
    let mut rng = Rng(0x1234_4321_abcd_dcba);
    let mut states = Vec::new();
    for _ in 0..CASES {
        let mut st = St::default();
        for r in 0..30 {
            st.gpr[r] = rng.word();
        }
        st.lr = 0x4;
        states.push(st);
    }
    let mut failures = Vec::new();
    for idx in 0..2 {
        let out = run_program(&exe, idx, &states).expect("the program ran");
        for (st, got) in states.iter().zip(&out) {
            let mut want = st.clone();
            want.gpr[3] = st.gpr[3].wrapping_add(3);
            want.gpr[5] = want.gpr[3].wrapping_mul(3);
            if idx == 1 {
                want.gpr[6] = want.gpr[5].wrapping_add(want.gpr[3]);
                want.lr = caller + 8;
            }
            if got.gpr[..30] != want.gpr[..30] || got.lr != want.lr {
                failures.push(format!(
                    "entry {idx}: got {:08x?} lr {:#x}, want {:08x?} lr {:#x}",
                    &got.gpr[..30],
                    got.lr,
                    &want.gpr[..30],
                    want.lr
                ));
                break;
            }
        }
    }
    let _ = std::fs::remove_dir_all(exe.parent().unwrap());
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}
