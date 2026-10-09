//! A way to run PowerPC instructions on QEMU's emulator and read back the state. QEMU is an independent implementation of the
//! instruction set, so what it gives is evidence about what the instructions mean that does not come from this code.
//!
//! A static 32-bit big-endian ELF is made for each test: a stub loads a whole register state from an array, runs the instructions
//! under test, saves the state, and loops over the cases; at the end it writes both arrays to standard output. No linker is needed.
//! Registers r30 and r31 belong to the stub, so tests do not use them.

#![allow(dead_code)]

use std::io::Write;
use std::process::{Command, Stdio};

pub const BASE: u32 = 0x1000_0000;
const CODE_AT: u32 = BASE + 0x100;
const CELL: u32 = BASE + 0x1000;
const END_CELL: u32 = BASE + 0x1004;
pub const IN_BASE: u32 = BASE + 0x2000;
/// Bytes of one state in the arrays.
pub const STATE: usize = 0x400;
pub const GPR_AT: usize = 0;
pub const FPR_AT: usize = 0x80;
pub const CR_AT: usize = 0x180;
pub const XER_AT: usize = 0x184;
pub const LR_AT: usize = 0x188;
pub const CTR_AT: usize = 0x18c;
pub const GQR_AT: usize = 0x1a0;
pub const SCRATCH_AT: usize = 0x200;
pub const PS1_AT: usize = 0x300;
pub const SCRATCH_LEN: usize = 256;

/// A register state and the scratch memory the instructions may touch.
#[derive(Clone, PartialEq)]
pub struct St {
    pub gpr: [u32; 32],
    pub fpr: [u64; 32],
    /// The second half of each register (paired singles) and the quantization registers: not part of what QEMU runs.
    pub ps1: [u64; 32],
    pub gqr: [u32; 8],
    pub cr: u32,
    pub xer: u32,
    pub lr: u32,
    pub ctr: u32,
    pub scratch: [u8; SCRATCH_LEN],
}

impl Default for St {
    fn default() -> St {
        St {
            gpr: [0; 32],
            fpr: [0; 32],
            ps1: [0; 32],
            gqr: [0; 8],
            cr: 0,
            xer: 0,
            lr: 0,
            ctr: 0,
            scratch: [0; SCRATCH_LEN],
        }
    }
}

impl St {
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut b = vec![0u8; STATE];
        for (i, v) in self.gpr.iter().enumerate() {
            b[GPR_AT + 4 * i..GPR_AT + 4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        for (i, v) in self.fpr.iter().enumerate() {
            b[FPR_AT + 8 * i..FPR_AT + 8 * i + 8].copy_from_slice(&v.to_be_bytes());
        }
        for (i, v) in self.ps1.iter().enumerate() {
            b[PS1_AT + 8 * i..PS1_AT + 8 * i + 8].copy_from_slice(&v.to_be_bytes());
        }
        for (i, v) in self.gqr.iter().enumerate() {
            b[GQR_AT + 4 * i..GQR_AT + 4 * i + 4].copy_from_slice(&v.to_be_bytes());
        }
        for (at, v) in [(CR_AT, self.cr), (XER_AT, self.xer), (LR_AT, self.lr), (CTR_AT, self.ctr)] {
            b[at..at + 4].copy_from_slice(&v.to_be_bytes());
        }
        b[SCRATCH_AT..SCRATCH_AT + SCRATCH_LEN].copy_from_slice(&self.scratch);
        b
    }

    pub fn from_bytes(b: &[u8]) -> St {
        let u32_at = |o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap());
        let mut s = St::default();
        for i in 0..32 {
            s.gpr[i] = u32_at(GPR_AT + 4 * i);
            s.fpr[i] = u64::from_be_bytes(b[FPR_AT + 8 * i..FPR_AT + 8 * i + 8].try_into().unwrap());
            s.ps1[i] = u64::from_be_bytes(b[PS1_AT + 8 * i..PS1_AT + 8 * i + 8].try_into().unwrap());
        }
        for i in 0..8 {
            s.gqr[i] = u32_at(GQR_AT + 4 * i);
        }
        s.cr = u32_at(CR_AT);
        s.xer = u32_at(XER_AT);
        s.lr = u32_at(LR_AT);
        s.ctr = u32_at(CTR_AT);
        s.scratch.copy_from_slice(&b[SCRATCH_AT..SCRATCH_AT + SCRATCH_LEN]);
        s
    }
}

/// The address of the scratch memory of case `i`.
pub fn scratch_addr(i: usize) -> u32 {
    IN_BASE + (i * STATE + SCRATCH_AT) as u32
}

// ---- a tiny assembler for the stub

fn hi(x: u32) -> u32 {
    (x >> 16) & 0xffff
}
fn lo(x: u32) -> u32 {
    x & 0xffff
}
fn lis(rd: u32, imm: u32) -> u32 {
    0x3c00_0000 | rd << 21 | (imm & 0xffff)
}
fn ori(ra: u32, rs: u32, imm: u32) -> u32 {
    0x6000_0000 | rs << 21 | ra << 16 | (imm & 0xffff)
}
fn addi(rd: u32, ra: u32, imm: i32) -> u32 {
    0x3800_0000 | rd << 21 | ra << 16 | (imm as u32 & 0xffff)
}
fn lwz(rd: u32, d: i32, ra: u32) -> u32 {
    0x8000_0000 | rd << 21 | ra << 16 | (d as u32 & 0xffff)
}
fn stw(rs: u32, d: i32, ra: u32) -> u32 {
    0x9000_0000 | rs << 21 | ra << 16 | (d as u32 & 0xffff)
}
fn lfd(fd: u32, d: i32, ra: u32) -> u32 {
    0xc800_0000 | fd << 21 | ra << 16 | (d as u32 & 0xffff)
}
fn stfd(fs: u32, d: i32, ra: u32) -> u32 {
    0xd800_0000 | fs << 21 | ra << 16 | (d as u32 & 0xffff)
}
fn spr_field(spr: u32) -> u32 {
    ((spr & 31) << 5) | (spr >> 5)
}
fn mtspr(spr: u32, rs: u32) -> u32 {
    0x7c00_03a6 | rs << 21 | spr_field(spr) << 11
}
fn mfspr(rd: u32, spr: u32) -> u32 {
    0x7c00_02a6 | rd << 21 | spr_field(spr) << 11
}
fn mtcrf(mask: u32, rs: u32) -> u32 {
    0x7c00_0120 | rs << 21 | mask << 12
}
fn mfcr(rd: u32) -> u32 {
    0x7c00_0026 | rd << 21
}
fn add(rd: u32, ra: u32, rb: u32) -> u32 {
    0x7c00_0214 | rd << 21 | ra << 16 | rb << 11
}
fn cmplw(ra: u32, rb: u32) -> u32 {
    0x7c00_0040 | ra << 16 | rb << 11
}
const SC: u32 = 0x4400_0002;

/// The words of the stub around `test`, and the address of the first of `test`'s words.
pub fn stub(test: &[u32], cases: usize) -> (Vec<u32>, u32) {
    let mut w: Vec<u32> = Vec::new();
    // r31 = this case's input state
    w.push(lis(31, hi(CELL)));
    w.push(lwz(31, lo(CELL) as i32, 31));
    w.push(addi(30, 0, 0));
    for i in 0..32 {
        w.push(lfd(i, (FPR_AT + 8 * i as usize) as i32, 31));
    }
    w.push(lwz(0, CR_AT as i32, 31));
    w.push(mtcrf(0xff, 0));
    w.push(lwz(0, XER_AT as i32, 31));
    w.push(mtspr(1, 0));
    w.push(lwz(0, LR_AT as i32, 31));
    w.push(mtspr(8, 0));
    w.push(lwz(0, CTR_AT as i32, 31));
    w.push(mtspr(9, 0));
    for i in 1..30 {
        w.push(lwz(i, (GPR_AT + 4 * i as usize) as i32, 31));
    }
    w.push(lwz(0, GPR_AT as i32, 31));
    let test_at = CODE_AT + 4 * w.len() as u32;
    w.extend_from_slice(test);
    // r30 = this case's output state
    let out_offset = (cases * STATE) as u32;
    w.push(lis(30, hi(out_offset)));
    w.push(ori(30, 30, lo(out_offset)));
    w.push(add(30, 30, 31));
    for i in 0..30 {
        w.push(stw(i, (GPR_AT + 4 * i as usize) as i32, 30));
    }
    for i in 0..32 {
        w.push(stfd(i, (FPR_AT + 8 * i as usize) as i32, 30));
    }
    w.push(mfcr(0));
    w.push(stw(0, CR_AT as i32, 30));
    for (spr, at) in [(1, XER_AT), (8, LR_AT), (9, CTR_AT)] {
        w.push(mfspr(0, spr));
        w.push(stw(0, at as i32, 30));
    }
    // the next case
    // (r0 cannot be the base of an addi: that reads as zero)
    w.push(lis(30, hi(CELL)));
    w.push(lwz(29, lo(CELL) as i32, 30));
    w.push(addi(29, 29, STATE as i32));
    w.push(stw(29, lo(CELL) as i32, 30));
    w.push(lwz(31, lo(END_CELL) as i32, 30));
    w.push(cmplw(29, 31));
    // blt back to the start: branch displacement in bytes, from this instruction
    let here = CODE_AT + 4 * w.len() as u32;
    let back = (CODE_AT as i64 - here as i64) as i32;
    w.push(0x4180_0000 | (back as u32 & 0xfffc));
    // write(1, in_array .. out_array end), exit(0)
    let len = (2 * cases * STATE) as u32;
    w.push(addi(0, 0, 4));
    w.push(addi(3, 0, 1));
    w.push(lis(4, hi(IN_BASE)));
    w.push(ori(4, 4, lo(IN_BASE)));
    w.push(lis(5, hi(len)));
    w.push(ori(5, 5, lo(len)));
    w.push(SC);
    w.push(addi(0, 0, 1));
    w.push(addi(3, 0, 0));
    w.push(SC);
    (w, test_at)
}

pub fn elf(code: &[u32], cases: &[St]) -> Vec<u8> {
    let in_len = cases.len() * STATE;
    let file_len = (IN_BASE - BASE) as usize + in_len;
    let mem_len = file_len + in_len;
    let mut f = vec![0u8; file_len];
    // ELF header
    f[..4].copy_from_slice(b"\x7fELF");
    f[4] = 1; // 32-bit
    f[5] = 2; // big-endian
    f[6] = 1;
    let put16 = |f: &mut Vec<u8>, o: usize, v: u16| f[o..o + 2].copy_from_slice(&v.to_be_bytes());
    let put32 = |f: &mut Vec<u8>, o: usize, v: u32| f[o..o + 4].copy_from_slice(&v.to_be_bytes());
    put16(&mut f, 16, 2); // executable
    put16(&mut f, 18, 20); // PowerPC
    put32(&mut f, 20, 1);
    put32(&mut f, 24, CODE_AT); // entry
    put32(&mut f, 28, 52); // program header
    put16(&mut f, 40, 52);
    put16(&mut f, 42, 32);
    put16(&mut f, 44, 1);
    // program header: one segment holding everything, writable and executable
    put32(&mut f, 52, 1);
    put32(&mut f, 52 + 4, 0);
    put32(&mut f, 52 + 8, BASE);
    put32(&mut f, 52 + 12, BASE);
    put32(&mut f, 52 + 16, file_len as u32);
    put32(&mut f, 52 + 20, mem_len as u32);
    put32(&mut f, 52 + 24, 7);
    put32(&mut f, 52 + 28, 0x1000);
    for (i, w) in code.iter().enumerate() {
        f[(CODE_AT - BASE) as usize + 4 * i..(CODE_AT - BASE) as usize + 4 * i + 4].copy_from_slice(&w.to_be_bytes());
    }
    assert!((CODE_AT - BASE) as usize + 4 * code.len() < 0x1000, "the stub is too long");
    put32(&mut f, (CELL - BASE) as usize, IN_BASE);
    put32(&mut f, (END_CELL - BASE) as usize, IN_BASE + in_len as u32);
    for (i, c) in cases.iter().enumerate() {
        let at = (IN_BASE - BASE) as usize + i * STATE;
        f[at..at + STATE].copy_from_slice(&c.to_bytes());
    }
    f
}

/// Whether QEMU's PowerPC user-mode emulator is installed.
pub fn qemu_available() -> bool {
    Command::new("qemu-ppc-static")
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// What QEMU leaves: for each case, the state and the address the test words were at. `None` if it could not run.
pub struct Run {
    pub after: Vec<St>,
}

/// Marks the program QEMU is given as executable (QEMU reads it itself, so this is a courtesy where the system has the bit at all).
#[cfg(unix)]
fn make_executable(path: &std::path::Path) -> Result<(), String> {
    let mut perms = std::fs::metadata(path).map_err(|e| e.to_string())?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(path, perms).map_err(|e| e.to_string())
}

#[cfg(not(unix))]
fn make_executable(_path: &std::path::Path) -> Result<(), String> {
    Ok(())
}

/// Run `test` on each of the `cases` on QEMU's 750.
pub fn run_qemu(test: &[u32], cases: &[St], tag: &str) -> Result<Run, String> {
    let (code, _) = stub(test, cases.len());
    let image = elf(&code, cases);
    let path = std::env::temp_dir().join(format!("mh3u-ppc-qemu-{}-{tag}", std::process::id()));
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&image))
        .map_err(|e| e.to_string())?;
    make_executable(&path)?;
    let out = Command::new("qemu-ppc-static")
        .args(["-cpu", "750"])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string());
    if std::env::var_os("MH3U_KEEP_ELF").is_some() {
        eprintln!("kept {}", path.display());
    } else {
        let _ = std::fs::remove_file(&path);
    }
    let out = out?;
    if !out.status.success() {
        return Err(format!("qemu stopped: {:?} {}", out.status, String::from_utf8_lossy(&out.stderr)));
    }
    let n = cases.len();
    if out.stdout.len() != 2 * n * STATE {
        return Err(format!("qemu wrote {} bytes, wanted {}", out.stdout.len(), 2 * n * STATE));
    }
    // the first half is the input array, which the test may have changed in its scratch memory; the second is the output array
    let mut after = Vec::new();
    for i in 0..n {
        let mut s = St::from_bytes(&out.stdout[(n + i) * STATE..(n + i + 1) * STATE]);
        s.scratch
            .copy_from_slice(&out.stdout[i * STATE + SCRATCH_AT..i * STATE + SCRATCH_AT + SCRATCH_LEN]);
        after.push(s);
    }
    Ok(Run { after })
}

/// The address the test words are at in the stub (so that a link register or a branch can be checked).
pub fn test_address() -> u32 {
    stub(&[0], 1).1
}

/// Run a program made of exactly `code` (no stub) and say how it ended: the exit code, and what it wrote.
pub fn run_raw(code: &[u32], cases: &[St], tag: &str) -> Result<(Option<i32>, Vec<u8>), String> {
    let image = elf(code, cases);
    let path = std::env::temp_dir().join(format!("mh3u-ppc-raw-{}-{tag}", std::process::id()));
    std::fs::File::create(&path)
        .and_then(|mut f| f.write_all(&image))
        .map_err(|e| e.to_string())?;
    make_executable(&path)?;
    let out = Command::new("qemu-ppc-static")
        .args(["-cpu", "750"])
        .arg(&path)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string());
    if std::env::var_os("MH3U_KEEP_ELF").is_some() {
        eprintln!("kept {}", path.display());
    } else {
        let _ = std::fs::remove_file(&path);
    }
    let out = out?;
    Ok((out.status.code(), out.stdout))
}

pub fn words_exit(code: u32) -> Vec<u32> {
    vec![addi(0, 0, 1), addi(3, 0, code as i32), SC]
}

pub fn state_bytes(s: &St) -> Vec<u8> {
    s.to_bytes()
}

pub fn state_from_bytes(b: &[u8]) -> St {
    St::from_bytes(b)
}
