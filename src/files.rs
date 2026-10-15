//! std::fs performs real WASI calls. The host owns the ChoreoFS objects.
use std::{fs::File, io, os::wasi::io::IntoRawFd, path::Path};

#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    fn fd_close(fd: u32) -> u32;
    fn path_open(
        fd: u32,
        lookup: u32,
        path: *const u8,
        length: usize,
        flags: u32,
        rights: u64,
        inheriting: u64,
        fd_flags: u32,
        opened: *mut u32,
    ) -> u32;
    fn fd_readdir(fd: u32, buffer: *mut u8, length: usize, cookie: u64, used: *mut usize) -> u32;
}

fn errno(code: u32) -> io::Result<()> {
    if code == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(code as i32))
    }
}

/// The runtime's projected directory reply is a 96-byte chunk. Request that
/// size explicitly, so a large libc buffer cannot mistake a chunk for EOF.
/// This is the same WASI/ChoreoFS operation, with no guest directory cache.
pub fn read_dir(path: &Path) -> io::Result<Vec<String>> {
    let path = path.as_os_str().as_encoded_bytes();
    let path = path.strip_prefix(b"/").unwrap_or(path);
    let mut fd = 0;
    // SAFETY: path and fd are valid disjoint guest regions. Descriptor 3 is
    // the supplied preopen; its projected owner decides whether open succeeds.
    errno(unsafe { path_open(3, 0, path.as_ptr(), path.len(), 2, 1 << 14, 0, 4, &mut fd) })?;
    let entries = read_entries(fd);
    // SAFETY: this call owns the descriptor from the successful path_open.
    let closed = errno(unsafe { fd_close(fd) });
    closed?;
    entries
}

fn read_entries(fd: u32) -> io::Result<Vec<String>> {
    let mut names = Vec::new();
    let mut cookie = 0;
    loop {
        let mut bytes = [0; 96];
        let mut used = 0;
        // SAFETY: the output buffer and used count are live, writable regions.
        errno(unsafe { fd_readdir(fd, bytes.as_mut_ptr(), bytes.len(), cookie, &mut used) })?;
        if used > bytes.len() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut offset = 0;
        while offset + 24 <= used {
            let next = u64::from_le_bytes(bytes[offset..offset + 8].try_into().unwrap());
            let length =
                u32::from_le_bytes(bytes[offset + 16..offset + 20].try_into().unwrap()) as usize;
            if offset + 24 + length > used {
                break;
            }
            let name = std::str::from_utf8(&bytes[offset + 24..offset + 24 + length])
                .map_err(|_| io::ErrorKind::InvalidData)?;
            names.push(name.to_owned());
            cookie = next;
            offset += 24 + length;
        }
        if used < bytes.len() {
            return Ok(names);
        }
        if offset == 0 {
            return Err(io::ErrorKind::InvalidData.into());
        }
    }
}

/// A shared service may commit on close. File::drop cannot return that error.
pub fn close(file: File) -> io::Result<()> {
    let fd = file.into_raw_fd();
    // SAFETY: into_raw_fd transfers this live descriptor to this one close.
    let errno = unsafe { fd_close(fd as u32) };
    if errno == 0 {
        Ok(())
    } else {
        Err(io::Error::from_raw_os_error(errno as i32))
    }
}
