//! The C compiler that builds the translated code, and what it needs on the command line for the system it builds for. `build-game`
//! and the tests that compile translated C both use this, so a library and a test program are built the same way.
//!
//! The generated C uses GNU C (`__builtin_*`, `__attribute__`, statement forms MSVC lacks), so it needs clang or GCC. On Windows that
//! is clang from LLVM (targeting MSVC, the usual install, or MinGW, as llvm-mingw does) or MinGW's GCC; Microsoft's `cl` cannot build
//! it, and `clang-cl` takes MSVC-style options this does not speak, so both are refused with a message.

use std::process::Command;

/// What the compiler builds for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Target {
    /// Linux and the other Unix systems: ELF shared objects, position-independent code, `libm`.
    Unix,
    /// Windows with Microsoft's C runtime (clang's default there): a DLL linked by `lld-link`, which comes with LLVM.
    WindowsMsvc,
    /// Windows with the MinGW runtime (llvm-mingw, MSYS2): a DLL.
    WindowsGnu,
}

#[derive(Debug, Clone)]
pub struct Toolchain {
    /// The compiler program.
    pub cc: String,
    pub clang: bool,
    pub target: Target,
}

fn runs(cc: &str) -> bool {
    Command::new(cc).arg("--version").output().is_ok_and(|o| o.status.success())
}

fn file_stem(cc: &str) -> String {
    std::path::Path::new(cc)
        .file_stem()
        .map(|s| s.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

impl Toolchain {
    /// Finds the compiler: `MH3U_CC` if it is set, otherwise clang, then (off Windows) `cc`, then gcc. `flags` are the flags the
    /// compiles will get (a `--target=` among them decides what is built, so a cross build can be checked).
    pub fn detect(flags: &[String]) -> Result<Toolchain, String> {
        let cc = match std::env::var("MH3U_CC") {
            Ok(cc) => {
                let stem = file_stem(&cc);
                if stem == "cl" {
                    return Err(format!(
                        "MH3U_CC={cc}: Microsoft's cl cannot compile the translated C (it is GNU C); install LLVM and use clang"
                    ));
                }
                if stem == "clang-cl" {
                    return Err(format!(
                        "MH3U_CC={cc}: clang-cl takes MSVC-style options; use clang.exe from the same LLVM install instead"
                    ));
                }
                if !runs(&cc) {
                    return Err(format!("MH3U_CC={cc} does not run"));
                }
                cc
            }
            Err(_) => {
                let candidates: &[&str] = if cfg!(windows) {
                    &["clang", "gcc"]
                } else {
                    &["clang", "cc", "gcc"]
                };
                match candidates.iter().find(|c| runs(c)) {
                    Some(c) => c.to_string(),
                    None if cfg!(windows) && Command::new("cl").output().is_ok() => {
                        return Err(
                            "only Microsoft's cl was found, and it cannot compile the translated C (it is GNU C): install LLVM \
                                    (winget install LLVM.LLVM) and make sure clang is on PATH"
                                .into(),
                        );
                    }
                    None => return Err("no C compiler found: install clang (LLVM)".into()),
                }
            }
        };
        let version = Command::new(&cc)
            .arg("--version")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).into_owned());
        let clang = version.is_ok_and(|v| v.contains("clang"));
        let machine = Command::new(&cc)
            .args(flags)
            .arg("-dumpmachine")
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_ascii_lowercase())
            .unwrap_or_default();
        Ok(Toolchain {
            cc,
            clang,
            target: target_of(&machine),
        })
    }

    /// What every compile of generated C gets besides `CFlags`.
    pub fn compile_args(&self) -> Vec<String> {
        match self.target {
            // a shared object needs position-independent code; only the exported entry points are visible
            Target::Unix => vec!["-fPIC".into(), "-fvisibility=hidden".into()],
            // a DLL exports what is marked `__declspec(dllexport)` (`GAME_API`); code is position-independent there anyway
            Target::WindowsMsvc | Target::WindowsGnu => Vec::new(),
        }
    }

    /// What every link of generated C (a library or a test program) gets.
    pub fn link_args(&self) -> Vec<String> {
        match self.target {
            Target::Unix => vec!["-lm".into()],
            // the maths functions are in the C runtime; clang links with lld-link (or MinGW's lld), which comes with LLVM and does LTO
            Target::WindowsMsvc | Target::WindowsGnu if self.clang => vec!["-fuse-ld=lld".into()],
            Target::WindowsMsvc | Target::WindowsGnu => Vec::new(),
        }
    }

    /// The file name of the translated library for this target.
    pub fn library_name(&self) -> &'static str {
        match self.target {
            Target::Unix => "libgame.so",
            Target::WindowsMsvc | Target::WindowsGnu => "game.dll",
        }
    }

    /// The suffix of a program for this target.
    pub fn exe_suffix(&self) -> &'static str {
        match self.target {
            Target::Unix => "",
            Target::WindowsMsvc | Target::WindowsGnu => ".exe",
        }
    }
}

/// The target of a compiler from what `-dumpmachine` says ("x86_64-pc-linux-gnu", "x86_64-pc-windows-msvc", "x86_64-w64-mingw32",
/// "x86_64-w64-windows-gnu"); nothing at all means the system this runs on.
pub fn target_of(machine: &str) -> Target {
    if machine.contains("windows-msvc") || (machine.contains("windows") && !machine.contains("gnu") && !machine.contains("mingw")) {
        Target::WindowsMsvc
    } else if machine.contains("mingw") || machine.contains("windows") {
        Target::WindowsGnu
    } else if machine.is_empty() && cfg!(windows) {
        Target::WindowsMsvc
    } else {
        Target::Unix
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_target_comes_from_the_machine_name() {
        assert_eq!(target_of("x86_64-pc-linux-gnu"), Target::Unix);
        assert_eq!(target_of("x86_64-pc-windows-msvc"), Target::WindowsMsvc);
        assert_eq!(target_of("x86_64-w64-mingw32"), Target::WindowsGnu);
        assert_eq!(target_of("x86_64-w64-windows-gnu"), Target::WindowsGnu);
        assert_eq!(target_of("aarch64-pc-windows-msvc"), Target::WindowsMsvc);
    }

    #[test]
    fn windows_gets_no_pic_and_no_libm() {
        let t = Toolchain {
            cc: "clang".into(),
            clang: true,
            target: Target::WindowsMsvc,
        };
        assert!(t.compile_args().is_empty());
        assert_eq!(t.link_args(), vec!["-fuse-ld=lld".to_string()]);
        assert_eq!(t.library_name(), "game.dll");
        let u = Toolchain {
            cc: "clang".into(),
            clang: true,
            target: Target::Unix,
        };
        assert!(u.compile_args().contains(&"-fPIC".to_string()));
        assert_eq!(u.link_args(), vec!["-lm".to_string()]);
    }
}
