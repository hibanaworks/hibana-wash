# hibana-wash

A small shell for [hibana-wasip1-runtime](https://github.com/hibanaworks/hibana-wasip1-runtime)
and ChoreoFS. It runs as a real WASI Preview 1 guest.

The shell reads scripts, expands variables, and runs commands. It has no
filesystem of its own. File and terminal I/O use six WASI imports. The host
passes those requests through projected Hibana Endpoints and uses ChoreoFS
to perform the admitted object operations.

```text
script -> hibana-wash -> WASI -> projected Endpoint -> ChoreoFS
```

The shell does not know the task, its participants, or its required order.
Those rules belong in the host's global protocol and localsides. Variables,
paths, and exit codes never grant permission to perform a shared effect.

## Scripts

```sh
message='hello from wash'
for name in first second; do
    printf '%s: %s\n' "$name" "$message" >> /work/notes
done

if cat /work/notes | grep second; then
    echo found
else
    echo missing
fi

cat /missing || echo 'try another path'
```

The host supplies the available objects and paths. It may expose private work
files and shared services through the same interface. No service name is
built into wash.

Supported syntax: single and double quotes, escapes, `$name`, `${name}`, `$?`,
assignments, newline and `;`, `&&`, `||`, `|`, `<`, `>`, `>>`, `if/then/else/fi`,
`for/in/do/done`, and `while/do/done`. `.` and `source` run a saved script.
The shell parses the entire script before executing it. A syntax error causes
no command in that script to run. A command error sets a nonzero exit status
and permits subsequent commands or a later corrected script.

Built-in commands:

- `echo`, `printf` (`%s`, `%d`, `%%`, `\n`, `\t`, `\r`, `\\`)
- `cat`, `cp`, `ls`, `pwd`, `cd`
- `grep LITERAL`, `wc` (byte count), `expr INTEGER OP INTEGER`, `read VARIABLE`
- `test`, `[`, `true`, `false`, `:`, `.`, `source`, `exit`, `help`

Pipelines use bounded byte buffers and run in the same guest. They do not fork
processes, and assignments made by `read` remain visible after a pipeline.
Variable expansion preserves each word. There is no implicit field splitting
or glob expansion. This is a small shell language, not a Bash or POSIX shell
implementation. It does not launch native programs or support background jobs,
command substitution, heredocs, functions, or regular expressions.

The initial profile uses one 64KiB memory page, no heap, scripts up to 4,096
bytes, 256 tokens, 128 syntax nodes, 32 arguments, and 16 variables. Pipeline
and file-input buffers hold at most 4,096 bytes. Host object capacities are
separate. These bounds are resource limits, never task authorization rules.

## Build

```sh
rustup target add --toolchain 1.95.0 wasm32-wasip1
sh scripts/build.sh
```

The result is `target/wasm32-wasip1/release/hibana-wash.wasm`. There are no
Rust dependencies or shell-library patches. Native execution is rejected at
compile time. Native tests exercise the parser only and supply no substitute FS.

Each stdin request is a four-byte little-endian byte count followed by that
many script bytes. This preserves multiline scripts. The guest emits a
`wash> ` prompt after each request. `exit N` ends the guest with status `N`.
An explicit `fd_close` completes each output redirect, so service-close errors
cannot disappear in a file destructor.

```sh
cargo +1.95.0 test --locked --lib
cargo +1.95.0 clippy --locked --target wasm32-wasip1 --release -- -D warnings
python3 tools/profile.py target/wasm32-wasip1/release/hibana-wash.wasm
```

The initial consumer tests execute scripts under Hibana 0.9.9 and the pinned
WASI runtime, through real WASI requests and ChoreoFS. They cover private work,
error recovery, early requests that must wait, rejected arguments, and a
build/test/deploy task. They are mechanism tests, not scored model experiments.

Licensed under Apache-2.0 or MIT, at your option.
