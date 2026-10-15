#![no_std]

mod syntax;

#[cfg(target_os = "wasi")]
mod shell;
#[cfg(target_os = "wasi")]
pub mod wasi;
#[cfg(target_os = "wasi")]
pub use shell::run;

#[cfg(not(target_os = "wasi"))]
pub fn check(source: &[u8]) -> Result<(), &'static str> {
    syntax::Tree::parse(source).map(|_| ())
}
