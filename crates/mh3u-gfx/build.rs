//! Stamps the build with the commit it was made from (`MH3U_BUILD_COMMIT`, "+changes" when the working tree differs from it), so a log
//! or a frame dump tells which code made it; and with a hash of the shader translator's and the graphics code's sources
//! (`MH3U_SHADER_SOURCE`), which names the on-disk caches of translated shaders and pipelines: they stay valid across commits that do
//! not change that code, and a build whose translator differs in any way does not read them.

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
    // FNV-1a over each file's path (relative to its crate) and contents, in a sorted order
    let mut files = Vec::new();
    for root in ["../mh3u-latte/src", "src"] {
        println!("cargo:rerun-if-changed={root}");
        collect(std::path::Path::new(root), root, &mut files);
    }
    files.sort();
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for (name, path) in &files {
        let bytes = std::fs::read(path).unwrap_or_default();
        for b in name.as_bytes().iter().chain([0u8].iter()).chain(bytes.iter()) {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    }
    println!("cargo:rustc-env=MH3U_SHADER_SOURCE={h:016x}");
}

fn collect(dir: &std::path::Path, root: &str, out: &mut Vec<(String, std::path::PathBuf)>) {
    let Ok(entries) = std::fs::read_dir(dir) else { return };
    for e in entries.flatten() {
        let path = e.path();
        if path.is_dir() {
            collect(&path, root, out);
        } else if path.extension().is_some_and(|x| x == "rs") {
            let name = path.strip_prefix(root).unwrap_or(&path).to_string_lossy().replace('\\', "/");
            out.push((format!("{root}/{name}"), path));
        }
    }
}
