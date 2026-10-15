#[cfg(not(target_os = "wasi"))]
compile_error!("hibana-wash runs only as a WASI guest with projected ChoreoFS I/O");

fn main() {
    std::process::exit(hibana_wash::run());
}
