//! Compiling the C of `translate` into a shared library, one file at a time on every core. A C compiler must be installed (clang is the
//! one the translated code is tested with; `cc` and `gcc` are tried after it; `MH3U_CC` names another).
//!
//! The flags that decide speed and meaning come from `mh3u_recomp::CFlags` (`-O2` by default; `--native` and `--lto` opt in), the
//! same ones the tests compile the translated C with. `MH3U_CFLAGS` adds more, split at spaces, to every compile and to the link.

use anyhow::{Context, Result, bail};
use mh3u_recomp::CFlags;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

#[cfg(windows)]
pub const LIBRARY: &str = "game.dll";
#[cfg(not(windows))]
pub const LIBRARY: &str = "libgame.so";

fn works(cc: &str) -> bool {
    Command::new(cc).arg("--version").output().is_ok_and(|o| o.status.success())
}

fn compiler() -> Result<String> {
    if let Ok(cc) = std::env::var("MH3U_CC") {
        return if works(&cc) { Ok(cc) } else { bail!("MH3U_CC={cc} does not run") };
    }
    ["clang", "cc", "gcc"]
        .into_iter()
        .find(|c| works(c))
        .map(str::to_string)
        .context("no C compiler found (install clang)")
}

/// Whether `cc` is clang (thin LTO needs it).
fn is_clang(cc: &str) -> bool {
    Command::new(cc)
        .arg("--version")
        .output()
        .is_ok_and(|o| String::from_utf8_lossy(&o.stdout).contains("clang"))
}

pub fn run(dir: &Path, flags: CFlags, watch: bool) -> Result<()> {
    let cc = compiler()?;
    if flags.lto && !is_clang(&cc) {
        bail!("--lto needs clang (and lld); {cc} is not clang");
    }
    let extra: Vec<String> = std::env::var("MH3U_CFLAGS")
        .unwrap_or_default()
        .split_whitespace()
        .map(str::to_string)
        .collect();
    let mut args = flags.args();
    args.extend(extra.iter().cloned());
    println!("compiling with {cc} {}", args.join(" "));
    let mut sources: Vec<PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "c"))
        .collect();
    sources.sort();
    if sources.is_empty() {
        bail!("no C files in {}", dir.display());
    }
    let queue = Mutex::new(sources.to_vec());
    let failures = Mutex::new(Vec::new());
    let started = std::time::Instant::now();
    std::thread::scope(|scope| {
        for _ in 0..std::thread::available_parallelism().map_or(4, usize::from) {
            scope.spawn(|| {
                loop {
                    // the lock is let go before the compiler runs, or the files would be built one at a time
                    let next = queue.lock().expect("queue").pop();
                    let Some(src) = next else { break };
                    let out = Command::new(&cc)
                        .args(["-w", "-fPIC", "-fvisibility=hidden", "-c"])
                        .args(&args)
                        .args(if watch { vec!["-DMH3U_WATCH"] } else { vec![] })
                        .arg("-I")
                        .arg(dir)
                        .arg(&src)
                        .arg("-o")
                        .arg(src.with_extension("o"))
                        .output();
                    match out {
                        Ok(o) if o.status.success() => {}
                        Ok(o) => {
                            failures
                                .lock()
                                .expect("failures")
                                .push(format!("{}: {}", src.display(), String::from_utf8_lossy(&o.stderr)))
                        }
                        Err(e) => failures.lock().expect("failures").push(format!("{}: {e}", src.display())),
                    }
                }
            });
        }
    });
    let failures = failures.into_inner().expect("failures");
    if let Some(first) = failures.first() {
        bail!(
            "{} files did not compile, the first: {}",
            failures.len(),
            first.chars().take(2000).collect::<String>()
        );
    }
    println!("compiled {} files in {:.0?}", sources.len(), started.elapsed());
    let started = std::time::Instant::now();
    let mut link = Command::new(&cc);
    link.arg("-shared").arg("-o").arg(dir.join(LIBRARY));
    if flags.lto {
        // the code is made at the link: it needs the optimisation flags again, and lld (which runs it on every core)
        link.arg("-fuse-ld=lld").args(&args);
    }
    for s in &sources {
        link.arg(s.with_extension("o"));
    }
    let out = link.arg("-lm").output()?;
    if !out.status.success() {
        bail!(
            "linking failed: {}",
            String::from_utf8_lossy(&out.stderr).chars().take(2000).collect::<String>()
        );
    }
    println!("linked {} in {:.0?}", dir.join(LIBRARY).display(), started.elapsed());
    Ok(())
}
