//! Every instruction of the instruction set (but the paired singles, which QEMU does not have, and the privileged ones) run on random
//! register states, on QEMU's PowerPC 750 and on `mh3u-ppc`'s interpreter; the results must be identical.

use mh3u_ppc::Op::{self, *};
use mh3u_testkit::harness::*;
use mh3u_testkit::*;

#[test]
fn every_instruction_gives_what_qemu_gives() {
    if !qemu_available() {
        eprintln!("skipped: qemu-ppc-static is not installed");
        return;
    }
    let mut rng = Rng(0x2545_f491_4f6c_dd1d);
    let mut failures: Vec<String> = Vec::new();
    let mut tested = 0;
    let mut skipped = Vec::new();
    for &op in Op::ALL {
        if matches!(op, B | Bc | Bclr | Bcctr | Lwarx | Stwcx) {
            continue; // sequences: in their own tests
        }
        if let Some(why) = skipped_reason(op) {
            skipped.push(format!("{}: {why}", op.name()));
            continue;
        }
        let tpl = template(op);
        for variant in 0..VARIANTS {
            // one word per variant, many states
            let first = build(op, tpl, &mut rng, 0);
            let word = first.word[0];
            let mut cases = Vec::new();
            for i in 0..CASES {
                // the same word, a new state (the constraints depend on the word's registers, so build again until it is this word)
                let mut c = build(op, tpl, &mut rng, i);
                c.word[0] = word;
                fix_state_for_word(op, word, &mut c.st, i, &mut rng);
                cases.push(c);
            }
            let states: Vec<St> = cases.iter().map(|c| c.st.clone()).collect();
            let run = match run_qemu(&[word], &states, &format!("{}-{variant}", op.name())) {
                Ok(r) => r,
                Err(e) => {
                    failures.push(format!("{} {word:#010x}: {e}", op.name()));
                    continue;
                }
            };
            for (i, c) in cases.iter().enumerate() {
                match interpret(&[word], &c.st, i) {
                    Ok((ours, outside)) => {
                        let mut d = differences(op, &run.after[i], &ours);
                        if outside > 0 {
                            d.push(format!("the interpreter touched memory outside the scratch area {outside} times"));
                        }
                        if !d.is_empty() {
                            failures.push(format!(
                                "{} {word:#010x} case {i}: {}\n    start: gpr {:x?}",
                                op.name(),
                                d.join("; "),
                                &c.st.gpr[..8]
                            ));
                            break;
                        }
                    }
                    Err(stop) => {
                        failures.push(format!("{} {word:#010x}: interpreter stopped with {stop:?}", op.name()));
                        break;
                    }
                }
            }
            tested += 1;
        }
    }
    eprintln!("{tested} words tested, skipped:\n  {}", skipped.join("\n  "));
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.iter().take(80).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn every_branch_form_goes_where_qemu_goes() {
    if !qemu_available() {
        return;
    }
    let mut rng = Rng(0x1234_5678_9abc_def1);
    let mut failures = Vec::new();
    let valid_bo: [u32; 17] = [0, 1, 2, 3, 4, 5, 8, 9, 10, 11, 12, 13, 16, 17, 18, 19, 20];
    let no_decrement: [u32; 5] = [4, 5, 12, 13, 20];
    let target = test_address() + 8;
    let mut count = 0;
    for variant in 0..120 {
        let lk = u32::from(variant % 2 == 1);
        let kind = variant % 4 / 2; // 0: bc and b, 1: the register ones
        let (branch, label) = if kind == 0 && variant % 8 < 2 {
            // b: forward by two instructions
            (0x4800_0000 | 8 | lk, "b")
        } else if kind == 0 {
            let bo = valid_bo[rng.below(valid_bo.len() as u32) as usize];
            (0x4000_0000 | bo << 21 | rng.below(32) << 16 | 8 | lk, "bc")
        } else if variant % 8 < 6 {
            let bo = valid_bo[rng.below(valid_bo.len() as u32) as usize];
            (0x4c00_0020 | bo << 21 | rng.below(32) << 16 | lk, "bclr")
        } else {
            let bo = no_decrement[rng.below(no_decrement.len() as u32) as usize];
            (0x4c00_0420 | bo << 21 | rng.below(32) << 16 | lk, "bcctr")
        };
        let mut words: Vec<u32> = vec![branch];
        words.push(marker(3));
        words.push(marker(4));
        let mut states = Vec::new();
        for _ in 0..CASES {
            let mut st = St::default();
            for r in 0..30 {
                st.gpr[r] = rng.word();
            }
            st.cr = rng.next_u64() as u32;
            st.xer = (rng.next_u64() as u32) & 0xe000_0000;
            st.ctr = rng.below(4);
            st.lr = target;
            if label == "bcctr" {
                st.ctr = target;
                st.lr = rng.word();
            }
            if label == "bclr" {
                st.ctr = rng.below(4);
            }
            states.push(st);
        }
        failures.extend(run_sequence(&format!("{label}-{variant}"), &words, &states, &|st| {
            format!("cr {:#010x} ctr {} lr {:#x}", st.cr, st.ctr, st.lr)
        }));
        count += 1;
    }
    eprintln!("{count} branch sequences tested");
    assert!(
        failures.is_empty(),
        "{} failures:\n{}",
        failures.len(),
        failures.iter().take(20).cloned().collect::<Vec<_>>().join("\n")
    );
}

#[test]
fn the_reservation_pair_stores_only_when_it_holds_the_reservation() {
    if !qemu_available() {
        return;
    }
    let mut rng = Rng(0xabcdef0123456789);
    let mut failures = Vec::new();
    for variant in 0..12 {
        let (rs, ra, rb) = (rng.below(25), 25 + rng.below(2), 28);
        // lwarx r_d, ra, rb; stwcx. rs, ra, rb: the same address; or the store alone, or with the load at another address
        let rd = rng.below(25);
        let lwarx = 0x7c00_0028 | rd << 21 | ra << 16 | rb << 11;
        let stwcx = 0x7c00_012d | rs << 21 | ra << 16 | rb << 11;
        let other = 0x7c00_0028 | rd << 21 | ra << 16 | 27 << 11;
        let words: Vec<u32> = match variant % 3 {
            0 => vec![lwarx, stwcx],
            1 => vec![stwcx],
            _ => vec![other, stwcx],
        };
        let mut states = Vec::new();
        for i in 0..CASES {
            let mut st = St::default();
            for r in 0..30 {
                st.gpr[r] = rng.word();
            }
            st.gpr[ra as usize] = scratch_addr(i) + 96;
            st.gpr[rb as usize] = 4 * rng.below(8);
            st.gpr[27] = 4 * rng.below(8) + 4;
            st.cr = rng.next_u64() as u32;
            st.xer = (rng.next_u64() as u32) & 0xe000_0000;
            for b in st.scratch.iter_mut() {
                *b = rng.next_u64() as u8;
            }
            states.push(st);
        }
        failures.extend(run_sequence(&format!("larx-{variant}"), &words, &states, &|st| {
            format!("cr {:#010x}", st.cr)
        }));
    }
    assert!(failures.is_empty(), "{} failures:\n{}", failures.len(), failures.join("\n"));
}

#[test]
fn the_estimates_are_close_to_what_qemu_gives() {
    if !qemu_available() {
        return;
    }
    let mut rng = Rng(0x0f1e_2d3c_4b5a_6978);
    // fres f2,f1 and frsqrte f2,f1
    for (name, word) in [("fres", 0xec40_0830u32), ("frsqrte", 0xfc40_0834u32)] {
        let mut states = Vec::new();
        for _ in 0..CASES {
            let mut st = St::default();
            // positive, normal, in the range where an estimate is meaningful
            let x = f32::from_bits(0x2000_0000 + rng.below(0x4000_0000));
            st.fpr[1] = f64::from(x).to_bits();
            states.push(st);
        }
        let run = run_qemu(&[word], &states, name).expect("qemu ran");
        for (i, st) in states.iter().enumerate() {
            let (ours, _) = interpret(&[word], st, i).expect("the interpreter ran");
            let (a, b) = (f64::from_bits(run.after[i].fpr[2]), f64::from_bits(ours.fpr[2]));
            let rel = ((a - b) / b).abs();
            assert!(
                rel < 1.0 / 4096.0,
                "{name} of {:e}: qemu {a:e}, interpreter {b:e}",
                f64::from_bits(st.fpr[1])
            );
        }
    }
}
