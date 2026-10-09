//! DSP-ADPCM: frames of 8 bytes, a header byte (predictor index in the high nibble, scale exponent in the low one) and 14 four-bit
//! samples. Each sample is the nibble, scaled, plus a prediction from the two before it with one of eight pairs of coefficients
//! (fixed point with 11 fractional bits).

/// A voice's decoder state: the coefficients, the header byte in use, and the last two samples.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Adpcm {
    pub coefs: [i16; 16],
    pub pred_scale: u16,
    pub yn1: i16,
    pub yn2: i16,
}

impl Adpcm {
    /// The sample a nibble (0 to 15) stands for, given the history; the history moves on.
    pub fn sample(&mut self, nibble: u8) -> i16 {
        let n = i32::from(((nibble << 4) as i8) >> 4);
        let scale = 1i32 << (self.pred_scale & 0xf);
        let index = usize::from((self.pred_scale >> 4) & 7);
        let (c1, c2) = (i32::from(self.coefs[index * 2]), i32::from(self.coefs[index * 2 + 1]));
        let v = ((n * scale) << 11) + c1 * i32::from(self.yn1) + c2 * i32::from(self.yn2);
        let s = ((v + 1024) >> 11).clamp(-32768, 32767) as i16;
        self.yn2 = self.yn1;
        self.yn1 = s;
        s
    }

    /// Whole frames decoded (each starts with its header byte).
    pub fn frames(&mut self, bytes: &[u8]) -> Vec<i16> {
        let mut out = Vec::with_capacity(bytes.len() / 8 * 14);
        for frame in bytes.as_chunks::<8>().0 {
            self.pred_scale = u16::from(frame[0]);
            for &b in &frame[1..] {
                out.push(self.sample(b >> 4));
                out.push(self.sample(b & 15));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn with_no_prediction_and_no_scale_a_nibble_is_its_own_value() {
        let mut d = Adpcm::default();
        // header 0x00, then the nibbles 0..15 (seven bytes give 14 of them)
        let out = d.frames(&[0x00, 0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd]);
        assert_eq!(out, [0, 1, 2, 3, 4, 5, 6, 7, -8, -7, -6, -5, -4, -3]);
    }

    #[test]
    fn the_scale_shifts_the_nibble() {
        let mut d = Adpcm::default();
        let out = d.frames(&[0x04, 0x1f, 0x70, 0x80, 0, 0, 0, 0]);
        assert_eq!(&out[..6], &[16, -16, 112, 0, -128, 0]);
    }

    #[test]
    fn a_coefficient_of_one_on_the_last_sample_integrates() {
        // pair 1: c1 = 1.0 (2048), c2 = 0
        let mut coefs = [0i16; 16];
        coefs[2] = 2048;
        let mut d = Adpcm { coefs, ..Adpcm::default() };
        let out = d.frames(&[0x10, 0x11, 0x11, 0xff, 0x00, 0x00, 0x00, 0x00]);
        assert_eq!(&out[..6], &[1, 2, 3, 4, 3, 2]);
        assert_eq!(out[13], 2);
        assert_eq!((d.yn1, d.yn2), (2, 2));
    }

    #[test]
    fn the_second_coefficient_reads_the_sample_before_last_and_rounding_is_to_nearest() {
        // pair 2: c1 = 0, c2 = -0.5 (-1024)
        let mut coefs = [0i16; 16];
        coefs[5] = -1024;
        let mut d = Adpcm {
            coefs,
            pred_scale: 0x20,
            yn1: 0,
            yn2: 7,
        };
        // -0.5 * 7 = -3.5, rounds (half up) to -3
        assert_eq!(d.sample(0), -3);
        // now yn2 = 0
        assert_eq!(d.sample(0), 0);
        assert_eq!(d.sample(0), 2, "-0.5 * -3 = 1.5 rounds to 2");
    }

    #[test]
    fn the_result_is_clamped_to_sixteen_bits() {
        let mut coefs = [0i16; 16];
        coefs[0] = 2048;
        let mut d = Adpcm {
            coefs,
            pred_scale: 0x0c,
            yn1: 30000,
            yn2: 0,
        };
        assert_eq!(d.sample(7), 32767);
        d.yn1 = -30000;
        assert_eq!(d.sample(8), -32768);
    }
}
