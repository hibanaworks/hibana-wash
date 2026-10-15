#![no_std]
#![no_main]

#[cfg(not(target_os = "wasi"))]
compile_error!("hibana-wash runs only as a WASI guest with projected ChoreoFS I/O");

#[unsafe(no_mangle)]
pub extern "C" fn _start() {
    hibana_wash::wasi::exit(hibana_wash::run());
}

#[panic_handler]
fn panic(_: &core::panic::PanicInfo<'_>) -> ! {
    hibana_wash::wasi::exit(70)
}

// LLVM may lower byte-slice equality to memcmp. Supply this computation in
// the guest rather than introducing a libc or non-WASI host import.
#[unsafe(no_mangle)]
unsafe extern "C" fn memcmp(left: *const u8, right: *const u8, count: usize) -> i32 {
    for index in 0..count {
        // SAFETY: the compiler-generated caller supplies readable regions of
        // count bytes. Volatile reads prevent lowering this definition to itself.
        let a = unsafe { left.add(index).read_volatile() };
        let b = unsafe { right.add(index).read_volatile() };
        if a != b {
            return a as i32 - b as i32;
        }
    }
    0
}
