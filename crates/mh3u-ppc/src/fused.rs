//! Fused multiply-add, correctly rounded on every platform. `f64::mul_add` and `f32::mul_add` are not: without the FMA target feature
//! they call the C library's `fma`/`fmaf`, and mingw-w64's own (the one linked against msvcrt, as in llvm-mingw's msvcrt build, since
//! msvcrt.dll has none) is off by a bit in some cases and gives NaN for a finite product plus an infinity. That made the interpreter,
//! not the translated C, the wrong side of `c_diff.rs` on Windows (`tests/fma_vectors.rs` in mh3u-testkit has those cases). These are the `libm` crate's, in plain Rust, the same everywhere. (Use these, not `mul_add`: `clippy.toml`
//! forbids it.)

/// `a * b + c` with one rounding.
pub fn fma(a: f64, b: f64, c: f64) -> f64 {
    libm::fma(a, b, c)
}

/// `a * b + c` with one rounding, in single precision.
pub fn fmaf(a: f32, b: f32, c: f32) -> f32 {
    libm::fmaf(a, b, c)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cases where Windows' C library was wrong (from `c_diff.rs` on Windows); the expected results are the exact ones, worked out
    /// with rational arithmetic.
    #[test]
    fn the_cases_windows_got_wrong_come_out_exact() {
        let d = f64::from_bits;
        // a 1-ulp difference (fmadd 0xfee2adfa case 1)
        assert_eq!(
            fma(d(0x1f8f7e39830394b9), d(0xc05edd2f1a9fbe77), d(0x99c4ae3ca75e0b64)).to_bits(),
            0x9ffe6004039c69f1
        );
        let s = |x: u64| d(x) as f32;
        let back = |x: f32| f64::from(x).to_bits();
        // single precision, 1 ulp (fmadds 0xedd3123a case 9)
        assert_eq!(
            back(fmaf(s(0x3ddb7cdfe0000000), s(0x3ff0000000000000), s(0x3ddb7cdfe0000000))),
            0x3deb7cdfe0000000
        );
        // a product that does not fit a single, minus infinity, is minus infinity, not NaN (fmsubs 0xee2f03b8 case 11)
        assert_eq!(
            back(fmaf(s(0x47efffffe0000000), s(0x47efffffe0000000), -s(0x7ff0000000000000))),
            0xfff0000000000000
        );
    }

    /// Against exact arithmetic on many random operands: a double's product is exact in twice the bits, so the check uses a
    /// reference made from an exact product and a sum rounded to odd (see `reference`).
    #[test]
    fn single_precision_is_exact_on_random_operands() {
        let mut x = 0x9e37_79b9_7f4a_7c15u64;
        let mut next = || {
            x ^= x << 13;
            x ^= x >> 7;
            x ^= x << 17;
            x
        };
        for _ in 0..200_000 {
            let mut op = || {
                let r = next();
                // a mantissa and an exponent near 1, so that the three meet and round
                f32::from_bits((r as u32 & 0x807f_ffff) | ((127 - 30 + (r >> 40) as u32 % 60) << 23))
            };
            let (a, b, c) = (op(), op(), op());
            assert_eq!(fmaf(a, b, c).to_bits(), reference(a, b, c).to_bits(), "{a:e} * {b:e} + {c:e}");
        }
    }

    /// `a * b + c` for singles, exactly rounded, without a C library: the product of two singles is exact as a double, and the double
    /// sum rounded to odd (made odd when it was not exact) then rounded to single is the exact sum rounded once.
    fn reference(a: f32, b: f32, c: f32) -> f32 {
        let p = f64::from(a) * f64::from(b);
        let c = f64::from(c);
        let s = p + c;
        // the error of the sum (two-sum)
        let bb = s - p;
        let err = (p - (s - bb)) + (c - bb);
        let s = if err != 0.0 && s.to_bits() & 1 == 0 {
            // towards the exact value, to the neighbour, which is odd
            let up = (err > 0.0) == (s > 0.0);
            f64::from_bits(if up { s.to_bits() + 1 } else { s.to_bits() - 1 })
        } else {
            s
        };
        s as f32
    }
}
