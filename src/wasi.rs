//! The only I/O implementation. No host adapter or guest filesystem exists.

pub type Result<T> = core::result::Result<T, &'static str>;
pub const READ: u64 = 1 << 1;
pub const WRITE: u64 = 1 << 6;
pub const READDIR: u64 = 1 << 14;

#[repr(C)]
struct IoVec {
    pointer: *const u8,
    length: usize,
}

#[link(wasm_import_module = "wasi_snapshot_preview1")]
unsafe extern "C" {
    fn fd_read(fd: u32, iovs: *const IoVec, count: usize, used: *mut usize) -> u32;
    fn fd_write(fd: u32, iovs: *const IoVec, count: usize, used: *mut usize) -> u32;
    fn fd_close(fd: u32) -> u32;
    fn path_open(
        fd: u32,
        lookup: u32,
        path: *const u8,
        length: usize,
        oflags: u32,
        base: u64,
        inheriting: u64,
        flags: u32,
        opened: *mut u32,
    ) -> u32;
    fn fd_readdir(fd: u32, bytes: *mut u8, length: usize, cookie: u64, used: *mut usize) -> u32;
    fn proc_exit(status: u32) -> !;
}

pub fn exit(status: u32) -> ! {
    // SAFETY: proc_exit has no memory arguments and does not return.
    unsafe { proc_exit(status) }
}

#[inline(never)]
pub fn read(fd: u32, bytes: &mut [u8]) -> Result<usize> {
    let mut used = 0;
    let vector = IoVec {
        pointer: bytes.as_mut_ptr(),
        length: bytes.len().min(96),
    };
    // SAFETY: the vector, output count and writable guest region are live and
    // disjoint. The runtime independently validates every memory range.
    let errno = unsafe { fd_read(fd, &vector, 1, &mut used) };
    if errno != 0 || used > vector.length {
        return Err("read denied");
    }
    Ok(used)
}

pub fn read_exact(fd: u32, mut bytes: &mut [u8]) -> Result<()> {
    while !bytes.is_empty() {
        let used = read(fd, bytes)?;
        if used == 0 {
            return Err("unexpected end of input");
        }
        bytes = &mut bytes[used..];
    }
    Ok(())
}

#[inline(never)]
pub fn write(fd: u32, mut bytes: &[u8]) -> Result<()> {
    while !bytes.is_empty() {
        let vector = IoVec {
            pointer: bytes.as_ptr(),
            length: bytes.len().min(96),
        };
        let mut used = 0;
        // SAFETY: the input region and count are valid for this synchronous
        // WASI invocation. No pointer is retained by the guest adapter.
        let errno = unsafe { fd_write(fd, &vector, 1, &mut used) };
        if errno != 0 || used == 0 || used > vector.length {
            return Err("write denied");
        }
        bytes = &bytes[used..];
    }
    Ok(())
}

#[inline(never)]
pub fn open(path: &[u8], rights: u64, flags: u32, fd_flags: u32) -> Result<u32> {
    let path = path.strip_prefix(b"/").unwrap_or(path);
    let mut fd = 0;
    // SAFETY: path and result are valid disjoint guest regions. Descriptor 3
    // is the supplied preopen. Only its projected owner can admit this request.
    let errno = unsafe {
        path_open(
            3,
            0,
            path.as_ptr(),
            path.len(),
            flags,
            rights,
            0,
            fd_flags,
            &mut fd,
        )
    };
    if errno != 0 {
        return Err("open denied");
    }
    Ok(fd)
}

pub fn close(fd: u32) -> Result<()> {
    // SAFETY: the fd was returned by a successful open. Closing a shared
    // service is observable and its errno must never be lost in a destructor.
    if unsafe { fd_close(fd) } != 0 {
        return Err("close denied");
    }
    Ok(())
}

pub fn readdir(fd: u32, bytes: &mut [u8; 96], cookie: u64) -> Result<usize> {
    let mut used = 0;
    // SAFETY: the complete output buffer and count are valid guest regions.
    if unsafe { fd_readdir(fd, bytes.as_mut_ptr(), bytes.len(), cookie, &mut used) } != 0
        || used > bytes.len()
    {
        return Err("directory read denied");
    }
    Ok(used)
}
