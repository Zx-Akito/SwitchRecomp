//! Guest file system: nn::fs and C stdio calls backed by host directories.
//!
//! Mounts map a guest name ("rom", a save-data name, a RAM disk) to a host directory.
//! The RomFS directory comes from `RECOMP_ROMFS` (default `romfs`), writable mounts live
//! under `RECOMP_SAVE_DIR` (default `save`). Guest paths look like `rom:/Data/level0`;
//! `..` components are rejected so the guest cannot leave a mount.
//!
//! File, FILE* and directory handles are opaque ids the guest never dereferences.

use crate::{mem_load_u8, mem_read_bytes, mem_store_u64, mem_write_bytes, Cpu, RuntimeResult};
use std::collections::HashMap;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};

const RESULT_SUCCESS: i64 = 0;
/// nn::fs::ResultPathNotFound (module 2, description 1).
const RESULT_PATH_NOT_FOUND: i64 = 0x202;
/// nn::fs::ResultPathAlreadyExists (module 2, description 2).
const RESULT_PATH_ALREADY_EXISTS: i64 = 0x402;
/// nn::fs::ResultTargetNotFound (module 2, description 1002): no add-on content.
const RESULT_TARGET_NOT_FOUND: i64 = 0x1F_4002;
/// Handle ids start here so they are never mistaken for null.
const HANDLE_BASE: u64 = 0x7000_0000;
/// sizeof(nn::fs::DirectoryEntry): name[0x301], pad[3], type (s8), pad[3], size (s64).
const DIRECTORY_ENTRY_SIZE: u64 = 0x310;

struct OpenFileState {
    file: File,
    mode: i64,
}

struct OpenDirState {
    entries: Vec<(String, bool, u64)>,
    next: usize,
}

#[derive(Default)]
struct State {
    mounts: HashMap<String, PathBuf>,
    files: HashMap<u64, OpenFileState>,
    dirs: HashMap<u64, OpenDirState>,
    next_handle: u64,
}

fn state() -> std::sync::MutexGuard<'static, State> {
    static STATE: OnceLock<Mutex<State>> = OnceLock::new();
    STATE.get_or_init(Default::default).lock().unwrap()
}

fn env_dir(var: &str, default: &str) -> PathBuf {
    std::env::var_os(var).map_or_else(|| PathBuf::from(default), PathBuf::from)
}

fn read_cstr(address: u64) -> RuntimeResult<String> {
    let mut bytes = Vec::new();
    loop {
        let byte = mem_load_u8(address + bytes.len() as u64)? as u8;
        if byte == 0 {
            break;
        }
        bytes.push(byte);
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

/// Host path for a guest path such as `rom:/Data/level0`, if the mount exists and the
/// path stays inside it.
fn resolve(guest: &str) -> Option<PathBuf> {
    let (mount, rest) = guest.split_once(':')?;
    let root = state().mounts.get(mount)?.clone();
    let rest = Path::new(rest.trim_start_matches('/'));
    if rest
        .components()
        .any(|c| !matches!(c, Component::Normal(_)))
    {
        return None;
    }
    Some(root.join(rest))
}

fn new_handle(state: &mut State) -> u64 {
    state.next_handle += 1;
    HANDLE_BASE + state.next_handle * 0x10
}

fn mount(name: &str, dir: PathBuf) {
    let _ = fs::create_dir_all(&dir);
    eprintln!("[fs] mount {name}: -> {}", dir.display());
    state().mounts.insert(name.to_string(), dir);
}

fn writable_mount(name: &str) -> PathBuf {
    env_dir("RECOMP_SAVE_DIR", "save").join(name)
}

fn open_host_file(path: &Path, mode: &str) -> Option<File> {
    let mut options = OpenOptions::new();
    match mode.trim_end_matches('b').trim_end_matches('t') {
        "r" => options.read(true),
        "r+" => options.read(true).write(true),
        "w" => options.write(true).create(true).truncate(true),
        "w+" => options.read(true).write(true).create(true).truncate(true),
        "a" => options.append(true).create(true),
        "a+" => options.read(true).append(true).create(true),
        _ => options.read(true),
    };
    options.open(path).ok()
}

fn with_file<T>(handle: u64, f: impl FnOnce(&mut OpenFileState) -> T) -> Option<T> {
    state().files.get_mut(&handle).map(f)
}

fn read_at(handle: u64, offset: u64, len: usize) -> Option<Vec<u8>> {
    with_file(handle, |open| {
        let mut buf = vec![0; len];
        open.file.seek(SeekFrom::Start(offset)).ok()?;
        let mut filled = 0;
        while filled < len {
            match open.file.read(&mut buf[filled..]) {
                Ok(0) => break,
                Ok(n) => filled += n,
                Err(_) => return None,
            }
        }
        buf.truncate(filled);
        Some(buf)
    })
    .flatten()
}

fn write_at(handle: u64, offset: Option<u64>, bytes: &[u8]) -> bool {
    with_file(handle, |open| {
        if let Some(offset) = offset {
            if open.file.seek(SeekFrom::Start(offset)).is_err() {
                return false;
            }
        }
        open.file.write_all(bytes).is_ok()
    })
    .unwrap_or(false)
}

/// Writes to an open FILE*/file handle; false if `handle` is not one (e.g. stdout).
pub(crate) fn write_stream(handle: u64, bytes: &[u8]) -> bool {
    write_at(handle, None, bytes)
}

fn list_dir(path: &Path, mode: i64) -> Option<Vec<(String, bool, u64)>> {
    let mut entries = Vec::new();
    for entry in fs::read_dir(path).ok()?.flatten() {
        let meta = entry.metadata().ok()?;
        // OpenDirectoryMode: 1 directories, 2 files, 3 both.
        if (meta.is_dir() && mode & 1 == 0) || (meta.is_file() && mode & 2 == 0) {
            continue;
        }
        entries.push((
            entry.file_name().to_string_lossy().into_owned(),
            meta.is_dir(),
            meta.len(),
        ));
    }
    entries.sort();
    Some(entries)
}

fn result(ok: bool, error: i64) -> i64 {
    if ok {
        RESULT_SUCCESS
    } else {
        error
    }
}

/// Handles `symbol` if it is a file-system import; `None` otherwise.
pub(crate) fn call(ctx: &mut Cpu, symbol: &str) -> Option<RuntimeResult<()>> {
    Some(dispatch(ctx, symbol).transpose()?.map(|value| {
        ctx.x[0] = value;
    }))
}

/// Returns the value for x0, or `Ok(None)` when `symbol` is not a file-system import.
fn dispatch(ctx: &mut Cpu, symbol: &str) -> RuntimeResult<Option<i64>> {
    let x = ctx.x;
    let path_arg = |i: usize| read_cstr(x[i] as u64);
    Ok(Some(match symbol {
        // --- mounts ---------------------------------------------------------------
        // void nn::fs::EnableGlobalFileDataCache(void*, size_t): host page cache suffices.
        "_ZN2nn2fs25EnableGlobalFileDataCacheEPvm" => x[0],
        // nn::Result nn::fs::QueryMountRomCacheSize(size_t*)
        "_ZN2nn2fs22QueryMountRomCacheSizeEPm" => {
            mem_store_u64(x[0] as u64, 0x1000)?;
            RESULT_SUCCESS
        }
        // nn::Result nn::fs::MountRom(const char* name, void* cache, size_t)
        "_ZN2nn2fs8MountRomEPKcPvm" => {
            mount(&path_arg(0)?, env_dir("RECOMP_ROMFS", "romfs"));
            RESULT_SUCCESS
        }
        // nn::Result nn::fs::MountSaveData(const char* name, const Uid&) (+ read-only form)
        "_ZN2nn2fs13MountSaveDataEPKcRKNS_7account3UidE"
        | "_ZN2nn2fs21MountSaveDataReadOnlyEPKcNS_13ApplicationIdERKNS_7account3UidE" => {
            let name = path_arg(0)?;
            mount(&name, writable_mount(&name));
            RESULT_SUCCESS
        }
        // size_t nn::fs::GetRamDiskBufferSize(size_t) / nn::Result MountRamDisk(name, buf, size)
        "_ZN2nn2fs20GetRamDiskBufferSizeEm" => x[0],
        "_ZN2nn2fs12MountRamDiskEPKcPvm" => {
            let name = path_arg(0)?;
            mount(&name, writable_mount(&format!("ramdisk-{name}")));
            RESULT_SUCCESS
        }
        // bool nn::fs::IsMounted(const char*)
        "_ZN2nn2fs9IsMountedEPKc" => state().mounts.contains_key(&path_arg(0)?) as i64,
        // void nn::fs::Unmount(const char*)
        "_ZN2nn2fs7UnmountEPKc" => {
            state().mounts.remove(&path_arg(0)?);
            x[0]
        }
        // Save data always exists; commits are immediate on the host.
        "_ZN2nn2fs14EnsureSaveDataERKNS_7account3UidE"
        | "_ZN2nn2fs6CommitEPKc"
        | "_ZN2nn2fs6CommitEPKPKci" => RESULT_SUCCESS,
        "_ZN2nn2fs18IsSaveDataExistingERKNS_7account3UidE"
        | "_ZN2nn2fs18IsSaveDataExistingENS_13ApplicationIdERKNS_7account3UidE" => 1,
        // No add-on content is installed.
        "_ZN2nn2fs31QueryMountAddOnContentCacheSizeEPmi"
        | "_ZN2nn2fs17MountAddOnContentEPKciPvm" => RESULT_TARGET_NOT_FOUND,
        // nn::Result nn::fs::GetFreeSpaceSize(s64* out, const char* path)
        "_ZN2nn2fs16GetFreeSpaceSizeEPlPKc" => {
            mem_store_u64(x[0] as u64, 0x4000_0000)?;
            RESULT_SUCCESS
        }

        // --- nn::fs files -----------------------------------------------------------
        // nn::Result nn::fs::GetEntryType(DirectoryEntryType* out, const char* path)
        "_ZN2nn2fs12GetEntryTypeEPNS0_18DirectoryEntryTypeEPKc" => {
            match resolve(&path_arg(1)?).and_then(|p| fs::metadata(p).ok()) {
                Some(meta) => {
                    let kind: u32 = if meta.is_dir() { 0 } else { 1 };
                    mem_write_bytes(x[0] as u64, &kind.to_le_bytes())?;
                    RESULT_SUCCESS
                }
                None => RESULT_PATH_NOT_FOUND,
            }
        }
        // nn::Result nn::fs::OpenFile(FileHandle* out, const char* path, int mode)
        "_ZN2nn2fs8OpenFileEPNS0_10FileHandleEPKci" => {
            let mode = x[2];
            let file = resolve(&path_arg(1)?).and_then(|p| {
                OpenOptions::new()
                    .read(mode & 1 != 0)
                    .write(mode & 2 != 0)
                    .append(mode & 4 != 0)
                    .open(p)
                    .ok()
            });
            match file {
                Some(file) => {
                    let mut st = state();
                    let handle = new_handle(&mut st);
                    st.files.insert(handle, OpenFileState { file, mode });
                    drop(st);
                    mem_store_u64(x[0] as u64, handle)?;
                    RESULT_SUCCESS
                }
                None => RESULT_PATH_NOT_FOUND,
            }
        }
        // nn::Result nn::fs::GetFileSize(s64* out, FileHandle)
        "_ZN2nn2fs11GetFileSizeEPlNS0_10FileHandleE" => {
            match with_file(x[1] as u64, |o| o.file.metadata().map(|m| m.len()).ok()).flatten() {
                Some(size) => {
                    mem_store_u64(x[0] as u64, size)?;
                    RESULT_SUCCESS
                }
                None => RESULT_PATH_NOT_FOUND,
            }
        }
        // nn::Result nn::fs::ReadFile(FileHandle, s64 offset, void* buf, size_t) (+ option)
        "_ZN2nn2fs8ReadFileENS0_10FileHandleElPvm"
        | "_ZN2nn2fs8ReadFileENS0_10FileHandleElPvmRKNS0_10ReadOptionE" => {
            match read_at(x[0] as u64, x[1] as u64, x[3] as usize) {
                Some(bytes) => {
                    mem_write_bytes(x[2] as u64, &bytes)?;
                    RESULT_SUCCESS
                }
                None => RESULT_PATH_NOT_FOUND,
            }
        }
        // nn::Result nn::fs::ReadFile(size_t* read, FileHandle, s64, void*, size_t) (+ option)
        "_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvm"
        | "_ZN2nn2fs8ReadFileEPmNS0_10FileHandleElPvmRKNS0_10ReadOptionE" => {
            match read_at(x[1] as u64, x[2] as u64, x[4] as usize) {
                Some(bytes) => {
                    mem_write_bytes(x[3] as u64, &bytes)?;
                    mem_store_u64(x[0] as u64, bytes.len() as u64)?;
                    RESULT_SUCCESS
                }
                None => RESULT_PATH_NOT_FOUND,
            }
        }
        // nn::Result nn::fs::WriteFile(FileHandle, s64, const void*, size_t, const WriteOption&)
        "_ZN2nn2fs9WriteFileENS0_10FileHandleElPKvmRKNS0_11WriteOptionE" => {
            let bytes = mem_read_bytes(x[2] as u64, x[3] as usize)?;
            result(
                write_at(x[0] as u64, Some(x[1] as u64), &bytes),
                RESULT_PATH_NOT_FOUND,
            )
        }
        // nn::Result nn::fs::SetFileSize(FileHandle, s64) / FlushFile(FileHandle)
        "_ZN2nn2fs11SetFileSizeENS0_10FileHandleEl" => result(
            with_file(x[0] as u64, |o| o.file.set_len(x[1] as u64).is_ok()).unwrap_or(false),
            RESULT_PATH_NOT_FOUND,
        ),
        "_ZN2nn2fs9FlushFileENS0_10FileHandleE" => RESULT_SUCCESS,
        // int nn::fs::GetFileOpenMode(FileHandle)
        "_ZN2nn2fs15GetFileOpenModeENS0_10FileHandleE" => {
            with_file(x[0] as u64, |o| o.mode).unwrap_or(0)
        }
        // void nn::fs::CloseFile(FileHandle)
        "_ZN2nn2fs9CloseFileENS0_10FileHandleE" => {
            state().files.remove(&(x[0] as u64));
            x[0]
        }
        // nn::Result nn::fs::CreateFile(const char*, s64 size)
        "_ZN2nn2fs10CreateFileEPKcl" => match resolve(&path_arg(0)?) {
            Some(p) if p.exists() => RESULT_PATH_ALREADY_EXISTS,
            Some(p) => result(
                File::create(&p)
                    .and_then(|f| f.set_len(x[1] as u64))
                    .is_ok(),
                RESULT_PATH_NOT_FOUND,
            ),
            None => RESULT_PATH_NOT_FOUND,
        },
        "_ZN2nn2fs10DeleteFileEPKc" => result(
            resolve(&path_arg(0)?).is_some_and(|p| fs::remove_file(p).is_ok()),
            RESULT_PATH_NOT_FOUND,
        ),
        "_ZN2nn2fs10RenameFileEPKcS2_" | "_ZN2nn2fs15RenameDirectoryEPKcS2_" => {
            let (from, to) = (resolve(&path_arg(0)?), resolve(&path_arg(1)?));
            result(
                matches!((from, to), (Some(a), Some(b)) if fs::rename(&a, &b).is_ok()),
                RESULT_PATH_NOT_FOUND,
            )
        }
        "_ZN2nn2fs15CreateDirectoryEPKc" => match resolve(&path_arg(0)?) {
            Some(p) if p.exists() => RESULT_PATH_ALREADY_EXISTS,
            Some(p) => result(fs::create_dir(p).is_ok(), RESULT_PATH_NOT_FOUND),
            None => RESULT_PATH_NOT_FOUND,
        },
        "_ZN2nn2fs15DeleteDirectoryEPKc" => result(
            resolve(&path_arg(0)?).is_some_and(|p| fs::remove_dir(p).is_ok()),
            RESULT_PATH_NOT_FOUND,
        ),
        "_ZN2nn2fs26DeleteDirectoryRecursivelyEPKc" => result(
            resolve(&path_arg(0)?).is_some_and(|p| fs::remove_dir_all(p).is_ok()),
            RESULT_PATH_NOT_FOUND,
        ),
        "_ZN2nn2fs25CleanDirectoryRecursivelyEPKc" => {
            let ok = resolve(&path_arg(0)?)
                .is_some_and(|p| fs::remove_dir_all(&p).is_ok() && fs::create_dir(&p).is_ok());
            result(ok, RESULT_PATH_NOT_FOUND)
        }

        // --- nn::fs directories ---------------------------------------------------------
        // nn::Result nn::fs::OpenDirectory(DirectoryHandle* out, const char* path, int mode)
        "_ZN2nn2fs13OpenDirectoryEPNS0_15DirectoryHandleEPKci" => {
            match resolve(&path_arg(1)?).and_then(|p| list_dir(&p, x[2])) {
                Some(entries) => {
                    let mut st = state();
                    let handle = new_handle(&mut st);
                    st.dirs.insert(handle, OpenDirState { entries, next: 0 });
                    drop(st);
                    mem_store_u64(x[0] as u64, handle)?;
                    RESULT_SUCCESS
                }
                None => RESULT_PATH_NOT_FOUND,
            }
        }
        // nn::Result nn::fs::GetDirectoryEntryCount(s64* out, DirectoryHandle)
        "_ZN2nn2fs22GetDirectoryEntryCountEPlNS0_15DirectoryHandleE" => {
            let count = state().dirs.get(&(x[1] as u64)).map(|d| d.entries.len());
            match count {
                Some(count) => {
                    mem_store_u64(x[0] as u64, count as u64)?;
                    RESULT_SUCCESS
                }
                None => RESULT_PATH_NOT_FOUND,
            }
        }
        // nn::Result nn::fs::ReadDirectory(s64* read, DirectoryEntry* out, DirectoryHandle, s64 max)
        "_ZN2nn2fs13ReadDirectoryEPlPNS0_14DirectoryEntryENS0_15DirectoryHandleEl" => {
            let batch = {
                let mut st = state();
                let Some(dir) = st.dirs.get_mut(&(x[2] as u64)) else {
                    return Ok(Some(RESULT_PATH_NOT_FOUND));
                };
                let end = (dir.next + x[3].max(0) as usize).min(dir.entries.len());
                let batch = dir.entries[dir.next..end].to_vec();
                dir.next = end;
                batch
            };
            for (i, (name, is_dir, size)) in batch.iter().enumerate() {
                let mut entry = vec![0_u8; DIRECTORY_ENTRY_SIZE as usize];
                let name = name.as_bytes();
                entry[..name.len().min(0x300)].copy_from_slice(&name[..name.len().min(0x300)]);
                entry[0x304] = if *is_dir { 0 } else { 1 };
                entry[0x308..0x310].copy_from_slice(&size.to_le_bytes());
                mem_write_bytes(x[1] as u64 + i as u64 * DIRECTORY_ENTRY_SIZE, &entry)?;
            }
            mem_store_u64(x[0] as u64, batch.len() as u64)?;
            RESULT_SUCCESS
        }
        // void nn::fs::CloseDirectory(DirectoryHandle)
        "_ZN2nn2fs14CloseDirectoryENS0_15DirectoryHandleE" => {
            state().dirs.remove(&(x[0] as u64));
            x[0]
        }

        // --- C stdio ----------------------------------------------------------------------
        // FILE* fopen(const char* path, const char* mode)
        "fopen" => {
            let (path, mode) = (path_arg(0)?, path_arg(1)?);
            match resolve(&path).and_then(|p| open_host_file(&p, &mode)) {
                Some(file) => {
                    let mut st = state();
                    let handle = new_handle(&mut st);
                    st.files.insert(handle, OpenFileState { file, mode: 0 });
                    handle as i64
                }
                None => {
                    eprintln!("[fs] fopen {path:?} ({mode}) failed");
                    0
                }
            }
        }
        // size_t fread(void* ptr, size_t size, size_t n, FILE*)
        "fread" => {
            let (size, n) = (x[1] as usize, x[2] as usize);
            let bytes = with_file(x[3] as u64, |o| {
                let mut buf = vec![0; size * n];
                let read = o.file.read(&mut buf).unwrap_or(0);
                buf.truncate(read);
                buf
            });
            match bytes {
                Some(bytes) => {
                    mem_write_bytes(x[0] as u64, &bytes)?;
                    (bytes.len() / size.max(1)) as i64
                }
                None => 0,
            }
        }
        // size_t fwrite(const void* ptr, size_t size, size_t n, FILE*)
        "fwrite" => {
            let bytes = mem_read_bytes(x[0] as u64, x[1] as usize * x[2] as usize)?;
            if write_at(x[3] as u64, None, &bytes) {
                x[2]
            } else {
                eprint!("{}", String::from_utf8_lossy(&bytes));
                x[2]
            }
        }
        // int fputs(const char*, FILE*): unknown streams (stdout/stderr) go to host stderr.
        "fputs" => {
            let text = path_arg(0)?;
            if !write_at(x[1] as u64, None, text.as_bytes()) {
                eprint!("{text}");
            }
            0
        }
        // int fseek/fseeko(FILE*, off_t, int whence)
        "fseek" | "fseeko" => {
            let pos = match x[2] {
                0 => SeekFrom::Start(x[1] as u64),
                1 => SeekFrom::Current(x[1]),
                _ => SeekFrom::End(x[1]),
            };
            let ok = with_file(x[0] as u64, |o| o.file.seek(pos).is_ok()).unwrap_or(false);
            if ok {
                0
            } else {
                -1
            }
        }
        // long ftell/ftello(FILE*)
        "ftell" | "ftello" => with_file(x[0] as u64, |o| {
            o.file.stream_position().map_or(-1, |p| p as i64)
        })
        .unwrap_or(-1),
        "fflush" => 0,
        // int fclose(FILE*)
        "fclose" => {
            if state().files.remove(&(x[0] as u64)).is_some() {
                0
            } else {
                -1
            }
        }
        // char* fgets(char* buf, int n, FILE*)
        "fgets" => {
            let limit = (x[1] as usize).saturating_sub(1);
            let line = with_file(x[2] as u64, |o| {
                let mut line = Vec::new();
                let mut byte = [0_u8];
                while line.len() < limit && o.file.read(&mut byte).unwrap_or(0) == 1 {
                    line.push(byte[0]);
                    if byte[0] == b'\n' {
                        break;
                    }
                }
                line
            })
            .unwrap_or_default();
            if line.is_empty() {
                0
            } else {
                let mut bytes = line;
                bytes.push(0);
                mem_write_bytes(x[0] as u64, &bytes)?;
                x[0]
            }
        }
        // int stat(const char* path, struct stat*) with musl's AArch64 layout.
        "stat" => match resolve(&path_arg(0)?).and_then(|p| fs::metadata(p).ok()) {
            Some(meta) => {
                let mut st = [0_u8; 128];
                let mode: u32 = if meta.is_dir() { 0o040755 } else { 0o100644 };
                st[16..20].copy_from_slice(&mode.to_le_bytes());
                st[20..24].copy_from_slice(&1_u32.to_le_bytes());
                st[48..56].copy_from_slice(&meta.len().to_le_bytes());
                mem_write_bytes(x[1] as u64, &st)?;
                0
            }
            None => -1,
        },
        "mkdir" => {
            let ok = resolve(&path_arg(0)?).is_some_and(|p| fs::create_dir(p).is_ok());
            if ok {
                0
            } else {
                -1
            }
        }
        "unlink" | "rmdir" => {
            let ok = resolve(&path_arg(0)?)
                .is_some_and(|p| fs::remove_file(&p).is_ok() || fs::remove_dir(&p).is_ok());
            if ok {
                0
            } else {
                -1
            }
        }
        "rename" => {
            let (from, to) = (resolve(&path_arg(0)?), resolve(&path_arg(1)?));
            if matches!((from, to), (Some(a), Some(b)) if fs::rename(&a, &b).is_ok()) {
                0
            } else {
                -1
            }
        }
        _ => return Ok(None),
    }))
}
