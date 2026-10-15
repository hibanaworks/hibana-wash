# hibana-wash

This repository owns a small WASI guest shell. Keep it dependency-free,
Rust 2024 / stable Rust 1.95, and use std for readable shell code.
The no_std/no_alloc constraints belong to Hibana core, not this guest.

The shell owns syntax, variables, command exit status, temporary pipeline
buffers, and computations. It has no filesystem, task model, task names,
permission table, stage flags, or coordination state. All file and terminal
operations use actual WASI imports. The host connects those requests to
projected Hibana Endpoints and ChoreoFS. Do not add host/native fallbacks,
another Wasm engine, shell libraries, or a guest in-memory filesystem.

Keep task ordering and authorization in the host's global choreography and
localside Endpoint operations. A successful open or a file's contents do not
authorize a shared effect. Preserve fd_close errors and ordinary command errors.

Test syntax natively, then test actual scripts through the real
hibana-wasip1-runtime and ChoreoFS in the consumer. Native parser tests and
successful Wasm compilation do not establish end-to-end execution.
Do not report model experiments or Bash/POSIX compatibility from these checks.

Do not use development subagents. Use Hayato Ohhashi <o8@vmm.dev> for commits.
