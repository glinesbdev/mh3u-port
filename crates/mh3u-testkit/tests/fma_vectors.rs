//! The fused multiply-adds on cases that came out wrong on Windows, where the interpreter's `mul_add` called the C library's `fma`
//! (mingw-w64's own, in llvm-mingw's msvcrt build), which is off by a bit and gives NaN for a finite product plus an infinity. Each is an
//! instruction word and register state from `c_diff.rs` (word and case number as it reported them), with the exact result (worked out
//! with rational arithmetic; the translated C gave it too). The interpreter must give these on every platform.

use mh3u_testkit::harness::St;
use mh3u_testkit::interpret;

struct V {
    word: u32,
    /// The operand registers' two halves (the register numbers are the word's fields).
    a: (u64, u64),
    b: (u64, u64),
    c: (u64, u64),
    out: (u64, u64),
}

const CASES: &[V] = &[
    // fmadd case 1: Windows gave 0x9ffe6004039c69f0 for f23
    V {
        word: 0xfee2adfa,
        a: (0x1f8f7e39830394b9, 0x47efffffe0000000),
        b: (0x99c4ae3ca75e0b64, 0xbff0000000000000),
        c: (0xc05edd2f1a9fbe77, 0x47efffffe0000000),
        out: (0x9ffe6004039c69f1, 0x47efffffe0000000),
    },
    // fmsub case 2: Windows gave 0x0000000000000000 for f6
    V {
        word: 0xfcc8cbf8,
        a: (0xb05d0a867a8e8cdd, 0x3be2b48500000000),
        b: (0x8000000000000000, 0x8000000000000000),
        c: (0x00000000000002dd, 0xc7649a93e0000000),
        out: (0x8000000000000000, 0x47efffffe0000000),
    },
    // fnmadd case 4: Windows gave 0x80000000000008df for f3
    V {
        word: 0xfc72b23e,
        a: (0x000000000000018f, 0x0000000000000000),
        b: (0x000000000000036b, 0x47efffffe0000000),
        c: (0x400c000000000000, 0xc2b7675540000000),
        out: (0x80000000000008e0, 0x47efffffe0000000),
    },
    // fmadds case 9: Windows gave 0x3deb7ce000000000 for f14
    V {
        word: 0xedd3123a,
        a: (0x3ddb7cdfe0000000, 0x4202a05f20000000),
        b: (0x3ddb7cdfe0000000, 0x3ddb7cdfe0000000),
        c: (0x3ff0000000000000, 0x36ec000000000000),
        out: (0x3deb7cdfe0000000, 0xc743c12ec0000000),
    },
    // fmsubs case 11: Windows gave 0x7ff8000000000000 for f17
    V {
        word: 0xee2f03b8,
        a: (0x47efffffe0000000, 0x47efffffe0000000),
        b: (0x7ff0000000000000, 0x0000000000000000),
        c: (0x47efffffe0000000, 0x4077dd7180000000),
        out: (0xfff0000000000000, 0xbac2aa8c00000000),
    },
    // fnmsubs case 14: Windows gave 0xfff0000000000000 for f14
    V {
        word: 0xedd65bfc,
        a: (0x4000000000000000, 0x3730600000000000),
        b: (0x47efffffe0000000, 0xc05edd2f20000000),
        c: (0x47efffffe0000000, 0x400921fb60000000),
        out: (0xc7efffffe0000000, 0x3a63a5a080000000),
    },
    // fnmadds case 21: Windows gave 0x7ff8000000000000 for f2
    V {
        word: 0xec5938be,
        a: (0x4202a05f20000000, 0x3f0dddb9e0000000),
        b: (0xfff0000000000000, 0xc05edd2f20000000),
        c: (0x47efffffe0000000, 0x3732380000000000),
        out: (0x7ff0000000000000, 0x3732380000000000),
    },
    // ps_madd case 3: Windows gave 0xc538207c40000000 for ps1 of f3
    V {
        word: 0x10627bfa,
        a: (0x7ff8000000000000, 0x44a11129a0000000),
        b: (0xc05edd2f1a9fbe77, 0xc0869e5400000000),
        c: (0xc05edd2f1a9fbe77, 0xc0869e5400000000),
        out: (0x7ff8000000000000, 0xc538207c60000000),
    },
    // ps_nmadd case 15: Windows gave 0x7ff8000000000000 for ps1 of f14
    V {
        word: 0x11d5727e,
        a: (0x00000000000003e5, 0x47efffffe0000000),
        b: (0x0000000000000334, 0xfff0000000000000),
        c: (0xf724a2862786ba42, 0x400921fb60000000),
        out: (0x7ff8000000000000, 0x7ff0000000000000),
    },
    // ps_nmadd case 4: Windows gave 0xc000001500000000 for f28
    V {
        word: 0x1390457e,
        a: (0xc41b0652926ec3bc, 0x3fe0000000000000),
        b: (0x4000000000000000, 0xbfda140040000000),
        c: (0xbad8f1e30de0c2e4, 0x3738f00000000000),
        out: (0xc000001520000000, 0x3fda140040000000),
    },
    // ps_madds0 case 10: Windows gave 0x45db7ceba0000000 for ps1 of f18
    V {
        word: 0x124f23dc,
        a: (0x3ddb7cdfd9d7bdbb, 0x47efffffe0000000),
        b: (0x4202a05f20000000, 0x44c7dae160000000),
        c: (0x3ddb7cdfd9d7bdbb, 0x47efffffe0000000),
        out: (0x4202a05f20000000, 0x45db7cebc0000000),
    },
    // ps_madds1 case 12: Windows gave 0x415a2c49a0000000 for f5
    V {
        word: 0x10a1c6de,
        a: (0x4202a05f20000000, 0x8000000000000000),
        b: (0x000000000000028a, 0x400921fb60000000),
        c: (0x768e116f0aec8066, 0x3f467b80c0000000),
        out: (0x415a2c4980000000, 0x400921fb60000000),
    },
    // fnmsub case 11: Windows gave 0x4fb1f6859cd335b7 for f6
    V {
        word: 0xfcd30b3c,
        a: (0xab3c75daf7504e10, 0x3705800000000000),
        b: (0x400c000000000000, 0xc6f2014640000000),
        c: (0x646432711f6b08e7, 0x3fe0000000000000),
        out: (0x4fb1f6859cd335b6, 0x400921fb60000000),
    },
];

#[test]
fn the_fused_multiply_adds_windows_got_wrong_are_exact() {
    for v in CASES {
        let (d, a, b, c) = ((v.word >> 21) & 31, (v.word >> 16) & 31, (v.word >> 11) & 31, (v.word >> 6) & 31);
        let mut st = St::default();
        // (a register can be named twice; the state the case had is what the last write leaves, as in c_diff.rs)
        for (r, (lo, hi)) in [(b, v.b), (c, v.c), (a, v.a)] {
            st.fpr[r as usize] = lo;
            st.ps1[r as usize] = hi;
        }
        let (out, _) = interpret(&[v.word], &st, 0).expect("runs");
        let ps = v.word >> 26 == 4;
        assert_eq!(out.fpr[d as usize], v.out.0, "{:#010x}: f{d}", v.word);
        if ps {
            assert_eq!(out.ps1[d as usize], v.out.1, "{:#010x}: ps1 of f{d}", v.word);
        }
    }
}
