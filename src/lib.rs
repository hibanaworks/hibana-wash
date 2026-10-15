mod syntax;

#[cfg(target_os = "wasi")]
mod files;
#[cfg(target_os = "wasi")]
mod shell;
#[cfg(target_os = "wasi")]
pub use shell::run;

#[cfg(not(target_os = "wasi"))]
pub fn check(source: &[u8]) -> Result<(), &'static str> {
    let source = std::str::from_utf8(source).map_err(|_| "script is not UTF-8")?;
    syntax::parse(source).map(|_| ())
}
