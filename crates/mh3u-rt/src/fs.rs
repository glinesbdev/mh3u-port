//! The file system (`FS*`) and the save functions (`SAVE*`), over the owner's own files.
//!
//! The game sees `/vol/content/...` (read only: the update's files first, then the base game's) and, through `SAVE*` calls, a save folder
//! that lives in the owner's data directory. Paths are matched without regard to case, as the console's file system does. All the calls
//! the game imports are the synchronous ones.

use crate::Ctx;
use crate::kernel::{Kernel, KernelHandler, Step};
use std::collections::HashMap;
use std::fs::File;
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};

const OK: i32 = 0;
const END: i32 = -2;
const EXISTS: i32 = -5;
const NOT_FOUND: i32 = -6;
const NOT_FILE: i32 = -7;
const NOT_DIR: i32 = -8;
const ACCESS_ERROR: i32 = -9;

/// Where the files are on the host.
#[derive(Default)]
pub struct Roots {
    /// The `content` folders, in the order they are searched.
    pub content: Vec<PathBuf>,
    /// Where the game's saves go.
    pub save: PathBuf,
}

enum Handle {
    File { file: File, writable: bool },
    Dir { entries: Vec<(String, PathBuf)>, next: usize },
}

#[derive(Default)]
pub struct Fs {
    pub roots: Roots,
    handles: HashMap<u32, Handle>,
    next: u32,
    user_data: HashMap<u32, u32>,
}

/// Finds `relative` below `root`, ignoring case in each component.
fn resolve(root: &Path, relative: &str) -> Option<PathBuf> {
    let mut at = root.to_path_buf();
    for part in relative.split('/').filter(|p| !p.is_empty() && *p != ".") {
        let exact = at.join(part);
        if exact.exists() {
            at = exact;
            continue;
        }
        let found = std::fs::read_dir(&at)
            .ok()?
            .filter_map(Result::ok)
            .find(|e| e.file_name().to_string_lossy().eq_ignore_ascii_case(part))?;
        at = found.path();
    }
    Some(at)
}

impl Fs {
    /// The host path of a game path, and whether it may be written.
    fn path(&self, game: &str) -> Option<(PathBuf, bool)> {
        let rest = game.strip_prefix("/vol/")?;
        if let Some(r) = rest.strip_prefix("content") {
            return self.roots.content.iter().find_map(|root| resolve(root, r)).map(|p| (p, false));
        }
        if let Some(r) = rest.strip_prefix("save") {
            // /vol/save/common/... and /vol/save/<account>/... are the one save folder
            let r = r.trim_start_matches('/');
            let r = r.split_once('/').map_or("", |(_, rest)| rest);
            return Some((resolve(&self.roots.save, r).unwrap_or_else(|| self.roots.save.join(r)), true));
        }
        None
    }

    fn save_path(&self, game: &str) -> PathBuf {
        let r = game.trim_start_matches('/');
        let r = r.strip_prefix("common/").or_else(|| r.strip_prefix("common")).unwrap_or(r);
        resolve(&self.roots.save, r).unwrap_or_else(|| self.roots.save.join(r))
    }

    fn insert(&mut self, h: Handle) -> u32 {
        self.next += 1;
        self.handles.insert(self.next, h);
        self.next
    }
}

/// Writes an `FSStat` (0x64 bytes) for `path`.
fn write_stat(k: &Kernel, at: u32, path: &Path) -> i32 {
    let Ok(meta) = std::fs::metadata(path) else { return NOT_FOUND };
    let g = k.guest;
    g.fill(at, 0, 0x64);
    // flags: 0x80000000 marks a directory; mode: read for everyone
    g.put_u32(at, if meta.is_dir() { 0x8000_0000 } else { 0 });
    g.put_u32(at + 4, 0x444);
    let size = if meta.is_dir() {
        0
    } else {
        meta.len().min(u64::from(u32::MAX)) as u32
    };
    g.put_u32(at + 0x10, size);
    g.put_u32(at + 0x14, size);
    OK
}

fn open_file(k: &mut Kernel, c: &mut Ctx, host: Option<(PathBuf, bool)>, mode: &str, out: u32) -> Step {
    let Some((path, may_write)) = host else {
        return result(c, NOT_FOUND);
    };
    let write = mode.contains('w') || mode.contains('a') || mode.contains('+');
    if write && !may_write {
        return result(c, ACCESS_ERROR);
    }
    if path.is_dir() {
        return result(c, NOT_FILE);
    }
    let file = if write {
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let mut o = std::fs::OpenOptions::new();
        o.read(true).write(true).create(true);
        if mode.starts_with('w') {
            o.truncate(true);
        }
        o.open(&path)
    } else {
        File::open(&path)
    };
    match file {
        Ok(file) => {
            let h = k.fs.insert(Handle::File { file, writable: write });
            k.guest.put_u32(out, h);
            result(c, OK)
        }
        Err(_) => result(c, NOT_FOUND),
    }
}

fn result(c: &mut Ctx, status: i32) -> Step {
    c.r[3] = status as u32;
    Step::Done
}

/// Reads `size * count` bytes into the guest at `buffer`; returns how many whole elements were read.
fn read_into(k: &mut Kernel, buffer: u32, size: u32, count: u32, handle: u32, at: Option<u32>) -> i32 {
    let g = k.guest;
    let Some(Handle::File { file, .. }) = k.fs.handles.get_mut(&handle) else {
        return NOT_FOUND;
    };
    if let Some(pos) = at
        && file.seek(SeekFrom::Start(u64::from(pos))).is_err()
    {
        return ACCESS_ERROR;
    }
    let want = u64::from(size) * u64::from(count);
    let mut bytes = Vec::new();
    if file.take(want).read_to_end(&mut bytes).is_err() {
        return ACCESS_ERROR;
    }
    g.write(buffer, &bytes);
    (bytes.len() as u32).checked_div(size).unwrap_or(0) as i32
}

pub fn find(name: &str) -> Option<KernelHandler> {
    Some(match name {
        "coreinit:FSInit" | "coreinit:FSShutdown" | "nn_save:SAVEInit" | "nn_save:SAVEShutdown" => |_, c| result(c, OK),
        "coreinit:FSAddClient" | "coreinit:FSDelClient" | "coreinit:FSSetStateChangeNotification" => |_, c| result(c, OK),
        "coreinit:FSInitCmdBlock" => |k, c| {
            k.guest.fill(c.r[3], 0, 0xa80);
            Step::Done
        },
        "coreinit:FSSetUserData" => |k, c| {
            k.fs.user_data.insert(c.r[3], c.r[4]);
            Step::Done
        },
        "coreinit:FSGetUserData" => |k, c| {
            c.r[3] = k.fs.user_data.get(&c.r[3]).copied().unwrap_or(0);
            Step::Done
        },
        "coreinit:FSGetLastError" | "coreinit:FSGetLastErrorCodeForViewer" | "coreinit:FSGetCurrentCmdBlock" => |_, c| result(c, 0),
        // (client, block, path, mode, *handle, error mask)
        "coreinit:FSOpenFile" => |k, c| {
            let (path, mode) = (k.guest.cstr(c.r[5]), k.guest.cstr(c.r[6]));
            let host = k.fs.path(&path);
            if k.report.trace {
                eprintln!("[fs] open {path} ({mode}): {}", if host.is_some() { "found" } else { "NOT FOUND" });
            }
            open_file(k, c, host, &mode, c.r[7])
        },
        // (client, block, account, path, mode, *handle, error mask)
        "nn_save:SAVEOpenFile" => |k, c| {
            let (path, mode) = (k.guest.cstr(c.r[6]), k.guest.cstr(c.r[7]));
            let host = Some((k.fs.save_path(&path), true));
            open_file(k, c, host, &mode, c.r[8])
        },
        "coreinit:FSCloseFile" | "coreinit:FSCloseDir" => |k, c| {
            let status = if k.fs.handles.remove(&c.r[5]).is_some() { OK } else { NOT_FOUND };
            result(c, status)
        },
        "coreinit:FSFlushFile" => |_, c| result(c, OK),
        // (client, block, buffer, size, count, handle, flags, error mask)
        "coreinit:FSReadFile" => |k, c| {
            let n = read_into(k, c.r[5], c.r[6], c.r[7], c.r[8], None);
            result(c, n)
        },
        // (client, block, buffer, size, count, position, handle, flags, error mask)
        "coreinit:FSReadFileWithPos" => |k, c| {
            let n = read_into(k, c.r[5], c.r[6], c.r[7], c.r[9], Some(c.r[8]));
            result(c, n)
        },
        "coreinit:FSWriteFile" => |k, c| {
            let (buffer, size, count, handle) = (c.r[5], c.r[6], c.r[7], c.r[8]);
            let g = k.guest;
            let Some(Handle::File { file, writable: true }) = k.fs.handles.get_mut(&handle) else {
                return result(c, ACCESS_ERROR);
            };
            let mut bytes = vec![0; (u64::from(size) * u64::from(count)) as usize];
            g.read(buffer, &mut bytes);
            let status = if file.write_all(&bytes).is_ok() {
                count as i32
            } else {
                ACCESS_ERROR
            };
            result(c, status)
        },
        // (client, block, handle, position, error mask)
        "coreinit:FSSetPosFile" => |k, c| {
            let status = match k.fs.handles.get_mut(&c.r[5]) {
                Some(Handle::File { file, .. }) => {
                    if file.seek(SeekFrom::Start(u64::from(c.r[6]))).is_ok() {
                        OK
                    } else {
                        ACCESS_ERROR
                    }
                }
                _ => NOT_FOUND,
            };
            result(c, status)
        },
        // (client, block, handle, *stat, error mask)
        "coreinit:FSGetStatFile" => |k, c| {
            let status = match k.fs.handles.get(&c.r[5]) {
                Some(Handle::File { file, .. }) => match file.metadata() {
                    Ok(m) => {
                        let g = k.guest;
                        g.fill(c.r[6], 0, 0x64);
                        g.put_u32(c.r[6] + 4, 0x444);
                        g.put_u32(c.r[6] + 0x10, m.len().min(u64::from(u32::MAX)) as u32);
                        g.put_u32(c.r[6] + 0x14, m.len().min(u64::from(u32::MAX)) as u32);
                        OK
                    }
                    Err(_) => ACCESS_ERROR,
                },
                _ => NOT_FOUND,
            };
            result(c, status)
        },
        // (client, block, path, *stat, error mask)
        "coreinit:FSGetStat" => |k, c| {
            let path = k.guest.cstr(c.r[5]);
            let status = k.fs.path(&path).map_or(NOT_FOUND, |(p, _)| write_stat(k, c.r[6], &p));
            if k.report.trace {
                eprintln!("[fs] stat {path}: {status}");
            }
            result(c, status)
        },
        // (client, block, account, path, *stat, error mask)
        "nn_save:SAVEGetStat" => |k, c| {
            let path = k.guest.cstr(c.r[6]);
            let host = k.fs.save_path(&path);
            result(c, write_stat(k, c.r[7], &host))
        },
        // (client, block, path, *handle, error mask)
        "coreinit:FSOpenDir" => |k, c| {
            let path = k.guest.cstr(c.r[5]);
            let Some((dir, _)) = k.fs.path(&path) else {
                return result(c, NOT_FOUND);
            };
            let Ok(read) = std::fs::read_dir(&dir) else {
                return result(c, if dir.exists() { NOT_DIR } else { NOT_FOUND });
            };
            let mut entries: Vec<(String, PathBuf)> = read
                .filter_map(Result::ok)
                .map(|e| (e.file_name().to_string_lossy().into_owned(), e.path()))
                .collect();
            entries.sort();
            let h = k.fs.insert(Handle::Dir { entries, next: 0 });
            k.guest.put_u32(c.r[6], h);
            result(c, OK)
        },
        // (client, block, handle, *entry, error mask): an FSStat, then the name
        "coreinit:FSReadDir" => |k, c| {
            let g = k.guest;
            let (entry, handle) = (c.r[6], c.r[5]);
            let Some(Handle::Dir { entries, next }) = k.fs.handles.get_mut(&handle) else {
                return result(c, NOT_FOUND);
            };
            let Some((name, path)) = entries.get(*next).cloned() else {
                return result(c, END);
            };
            *next += 1;
            let status = write_stat(k, entry, &path);
            g.fill(entry + 0x64, 0, 256);
            g.write(entry + 0x64, &name.as_bytes()[..name.len().min(255)]);
            result(c, status)
        },
        // (client, block, path, error mask) and (client, block, account, path, error mask)
        "coreinit:FSMakeDir" => |k, c| {
            let path = k.guest.cstr(c.r[5]);
            let status = match k.fs.path(&path) {
                Some((p, true)) => match std::fs::create_dir_all(&p) {
                    Ok(()) => OK,
                    Err(_) => EXISTS,
                },
                _ => ACCESS_ERROR,
            };
            result(c, status)
        },
        "nn_save:SAVEMakeDir" => |k, c| {
            let path = k.guest.cstr(c.r[6]);
            let status = if std::fs::create_dir_all(k.fs.save_path(&path)).is_ok() {
                OK
            } else {
                ACCESS_ERROR
            };
            result(c, status)
        },
        "coreinit:FSRemove" => |k, c| {
            let path = k.guest.cstr(c.r[5]);
            let status = match k.fs.path(&path) {
                Some((p, true)) => remove(&p),
                _ => ACCESS_ERROR,
            };
            result(c, status)
        },
        "nn_save:SAVERemove" => |k, c| {
            let path = k.guest.cstr(c.r[6]);
            result(c, remove(&k.fs.save_path(&path)))
        },
        "nn_save:SAVEInitSaveDir" | "nn_save:SAVEFlushQuota" => |_, c| result(c, OK),
        // (client, block, account, *u64 size, error mask): plenty
        "nn_save:SAVEGetFreeSpaceSize" => |k, c| {
            k.guest.write(c.r[6], &(1u64 << 32).to_be_bytes());
            result(c, OK)
        },
        _ => return None,
    })
}

fn remove(p: &Path) -> i32 {
    let done = if p.is_dir() {
        std::fs::remove_dir(p)
    } else {
        std::fs::remove_file(p)
    };
    if done.is_ok() { OK } else { NOT_FOUND }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_are_matched_without_regard_to_case() {
        let dir = std::env::temp_dir().join(format!("mh3u-fs-test-{}", std::process::id()));
        std::fs::create_dir_all(dir.join("Sub")).unwrap();
        std::fs::write(dir.join("Sub/File.Arc"), b"x").unwrap();
        assert_eq!(resolve(&dir, "/sub/file.arc"), Some(dir.join("Sub/File.Arc")));
        assert_eq!(resolve(&dir, "/sub/missing"), None);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn game_paths_map_to_the_roots_and_content_is_read_only() {
        let fs = Fs {
            roots: Roots {
                content: vec![PathBuf::from("/nonexistent")],
                save: PathBuf::from("/data/save"),
            },
            ..Fs::default()
        };
        assert_eq!(fs.path("/vol/content/x"), None);
        assert_eq!(fs.path("/vol/save/common/a/b"), Some((PathBuf::from("/data/save/a/b"), true)));
        assert_eq!(fs.path("/elsewhere"), None);
    }
}
