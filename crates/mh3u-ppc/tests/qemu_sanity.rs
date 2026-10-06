//! The harness itself: a program whose answer is known must come out of QEMU as expected.

mod harness;
use harness::*;

#[test]
fn qemu_runs_the_stub_and_returns_the_state() {
    if !qemu_available() {
        eprintln!("skipped: qemu-ppc-static is not installed");
        return;
    }
    let mut a = St::default();
    a.gpr[1] = 40;
    a.gpr[2] = 2;
    a.cr = 0x1234_5678;
    a.lr = 0xdead_beef;
    a.fpr[3] = 0x4009_21fb_5444_2d18;
    let mut b = a.clone();
    b.gpr[1] = 7;
    b.gpr[2] = 0xffff_ffff;
    // add r3, r1, r2
    let run = run_qemu(&[0x7c61_1214], &[a.clone(), b.clone()], "sanity").expect("qemu ran");
    assert_eq!(run.after[0].gpr[3], 42);
    assert_eq!(run.after[1].gpr[3], 6, "7 + 0xffffffff wraps");
    assert_eq!(run.after[0].gpr[1], 40, "the registers come back as they were");
    assert_eq!((run.after[0].cr, run.after[0].lr), (0x1234_5678, 0xdead_beef));
    assert_eq!(run.after[0].fpr[3], 0x4009_21fb_5444_2d18);
    // stw r1, 0(r2) with r2 pointing at the scratch memory changes the scratch memory
    let mut c = St::default();
    c.gpr[1] = 0x0102_0304;
    c.gpr[2] = scratch_addr(0) + 8;
    let run = run_qemu(&[0x9022_0000], &[c], "sanity2").expect("qemu ran");
    assert_eq!(&run.after[0].scratch[8..12], &[1, 2, 3, 4]);
}

#[test]
fn qemu_loads_the_made_up_elf_and_runs_a_syscall() {
    if !qemu_available() {
        return;
    }
    let (code, _) = run_raw(&words_exit(42), &[St::default()], "exit").unwrap();
    assert_eq!(code, Some(42));
}
