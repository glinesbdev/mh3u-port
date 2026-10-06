//! Compiling the C of `translate` into a shared library, one file at a time on every core. A C compiler must be installed (clang is the
//! one the translated code is tested with; `cc` and `gcc` are tried after it).

use anyhow::{Context, Result, bail};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

#[cfg(windows)]
pub const LIBRARY: &str = "game.dll";
#[cfg(not(windows))]
pub const LIBRARY: &str = "libgame.so";

fn compiler() -> Result<&'static str> {
    ["clang", "cc", "gcc"]
        .into_iter()
        .find(|c| Command::new(c).arg("--version").output().is_ok_and(|o| o.status.success()))
        .context("no C compiler found (install clang)")
}

pub fn run(dir: &Path, opt: u8) -> Result<()> {
    let cc = compiler()?;
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
                    let out = Command::new(cc)
                        .args([
                            "-w",
                            "-fPIC",
                            "-fvisibility=hidden",
                            "-fno-strict-aliasing",
                            "-c",
                            &format!("-O{opt}"),
                            "-I",
                        ])
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
    let mut link = Command::new(cc);
    link.arg("-shared").arg("-o").arg(dir.join(LIBRARY));
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
    println!("linked {}", dir.join(LIBRARY).display());
    Ok(())
}
