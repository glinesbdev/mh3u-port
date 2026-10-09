//! Stamps the build with the commit it was made from (`MH3U_BUILD_COMMIT`, "+changes" when the working tree differs from it), so a log
//! or a frame dump tells which code made it.

use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let out = Command::new("git").args(args).output().ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

fn main() {
    let commit = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let changed = git(&["status", "--porcelain", "--untracked-files=no"]).is_some_and(|s| !s.is_empty());
    println!(
        "cargo:rustc-env=MH3U_BUILD_COMMIT={commit}{}",
        if changed { "+changes" } else { "" }
    );
    for path in ["../../.git/HEAD", "../../.git/index", "../../.git/refs/heads"] {
        println!("cargo:rerun-if-changed={path}");
    }
}
