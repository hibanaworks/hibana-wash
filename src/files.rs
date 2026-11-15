//! std::fs performs real WASI calls. The host owns the ChoreoFS objects.
use std::{
    fs::File,
    io::{self, Read, Write},
    os::wasi::io::{FromRawFd, IntoRawFd},
    path::Path,
};

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

/// Request the standard WASI NONBLOCK flag; readiness and admission remain
/// with the actual projected file. An empty receive returns EAGAIN, not EOF.
pub fn read_nonblocking(path: &Path) -> io::Result<Vec<u8>> {
    let path = path.as_os_str().as_encoded_bytes();
    let path = path.strip_prefix(b"/").unwrap_or(path);
    let mut fd = 0;
    // SAFETY: the live path and output descriptor occupy disjoint regions.
    errno(unsafe { path_open(3, 0, path.as_ptr(), path.len(), 0, 2, 0, 4, &mut fd) })?;
    // SAFETY: successful path_open issued this descriptor to this call.
    let mut file = unsafe { File::from_raw_fd(fd as i32) };
    let mut bytes = Vec::new();
    // Read the message as a stream. File::read_to_end may seek to estimate
    // capacity; a projected message file has no seek operation.
    let read = (|| {
        let mut chunk = [0; 1024];
        loop {
            match file.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(count) => bytes.extend_from_slice(&chunk[..count]),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    })();
    let closed = close(file);
    closed?;
    read?;
    Ok(bytes)
}

/// Standard NONBLOCK output. A failed close remains visible to the shell;
/// descriptor drop cannot silently discard an admission refusal.
pub fn write_nonblocking(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let path = path.as_os_str().as_encoded_bytes();
    let path = path.strip_prefix(b"/").unwrap_or(path);
    let mut fd = 0;
    // SAFETY: the path and output descriptor are disjoint live guest regions.
    // CREAT | TRUNC, FD_WRITE and NONBLOCK are ordinary WASI flags/rights.
    errno(unsafe { path_open(3, 0, path.as_ptr(), path.len(), 9, 64, 0, 4, &mut fd) })?;
    // SAFETY: this successful path_open returned the descriptor to this call.
    let mut file = unsafe { File::from_raw_fd(fd as i32) };
    let written = file.write_all(bytes);
    let closed = close(file);
    written?;
    closed
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
