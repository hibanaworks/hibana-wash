use crate::{
    syntax::{self, Kind, NONE, Node, TokenKind, Tree},
    wasi::{self, Result},
};

const BYTES: usize = 4096;
const ARGS: usize = 32;

struct Bytes<const CAPACITY: usize = BYTES> {
    bytes: [u8; CAPACITY],
    len: usize,
}
impl<const CAPACITY: usize> Bytes<CAPACITY> {
    const fn new() -> Self {
        Self {
            bytes: [0; CAPACITY],
            len: 0,
        }
    }
    fn push(&mut self, bytes: &[u8]) -> Result<()> {
        let end = self
            .len
            .checked_add(bytes.len())
            .filter(|end| *end <= CAPACITY)
            .ok_or("shell buffer full")?;
        self.bytes[self.len..end].copy_from_slice(bytes);
        self.len = end;
        Ok(())
    }
    fn get(&self) -> &[u8] {
        &self.bytes[..self.len]
    }
}

enum Output<'a> {
    Fd(u32),
    Buffer(&'a mut Bytes),
}
impl Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        match self {
            Self::Fd(fd) => wasi::write(*fd, bytes),
            Self::Buffer(buffer) => buffer.push(bytes),
        }
    }
}

#[derive(Clone, Copy)]
struct Span {
    start: u16,
    end: u16,
}
impl Span {
    const EMPTY: Self = Self { start: 0, end: 0 };
}
struct Arguments {
    bytes: Bytes<2048>,
    spans: [Span; ARGS],
    len: usize,
}
impl Arguments {
    fn new() -> Self {
        Self {
            bytes: Bytes::new(),
            spans: [Span::EMPTY; ARGS],
            len: 0,
        }
    }
    fn push(&mut self, value: &[u8]) -> Result<()> {
        if self.len == ARGS {
            return Err("too many command arguments");
        }
        let start = self.bytes.len as u16;
        self.bytes.push(value)?;
        self.spans[self.len] = Span {
            start,
            end: self.bytes.len as u16,
        };
        self.len += 1;
        Ok(())
    }
    fn get(&self, index: usize) -> &[u8] {
        if index >= self.len {
            return b"";
        }
        let span = self.spans[index];
        &self.bytes.bytes[span.start as usize..span.end as usize]
    }
}

struct Variable {
    name: [u8; 32],
    name_len: usize,
    value: [u8; 256],
    value_len: usize,
}
impl Variable {
    const EMPTY: Self = Self {
        name: [0; 32],
        name_len: 0,
        value: [0; 256],
        value_len: 0,
    };
}
struct Shell {
    variables: [Variable; 16],
    cwd: [u8; 256],
    cwd_len: usize,
    status: u8,
}
#[derive(Clone, Copy)]
enum Control {
    Status(u8),
    Exit(u8),
}

impl Shell {
    fn new() -> Self {
        let mut cwd = [0; 256];
        cwd[0] = b'/';
        Self {
            variables: [const { Variable::EMPTY }; 16],
            cwd,
            cwd_len: 1,
            status: 0,
        }
    }
    fn variable(&self, name: &[u8]) -> &[u8] {
        if name == b"PWD" {
            return &self.cwd[..self.cwd_len];
        }
        self.variables
            .iter()
            .find(|item| &item.name[..item.name_len] == name)
            .map(|item| &item.value[..item.value_len])
            .unwrap_or(b"")
    }
    fn assign(&mut self, name: &[u8], value: &[u8]) -> Result<()> {
        if !syntax::name(name) || name.len() > 32 || value.len() > 256 {
            return Err("invalid or oversized variable");
        }
        let index = self
            .variables
            .iter()
            .position(|item| &item.name[..item.name_len] == name)
            .or_else(|| self.variables.iter().position(|item| item.name_len == 0))
            .ok_or("variable storage full")?;
        let item = &mut self.variables[index];
        item.name[..name.len()].copy_from_slice(name);
        item.name_len = name.len();
        item.value[..value.len()].copy_from_slice(value);
        item.value_len = value.len();
        Ok(())
    }

    #[inline(never)]
    fn expand<const N: usize>(&self, source: &[u8], output: &mut Bytes<N>) -> Result<()> {
        let mut quote = 0;
        let mut i = 0;
        while i < source.len() {
            let byte = source[i];
            if byte == b'\'' || byte == b'"' {
                if quote == byte {
                    quote = 0;
                    i += 1;
                    continue;
                }
                if quote == 0 {
                    quote = byte;
                    i += 1;
                    continue;
                }
            }
            if byte == b'\\' && quote != b'\'' {
                i += 1;
                let byte = *source.get(i).ok_or("unfinished escape")?;
                if byte != b'\n' {
                    output.push(&[byte])?;
                }
            } else if byte == b'$' && quote != b'\'' {
                i += 1;
                if source.get(i) == Some(&b'?') {
                    let mut status = [0u8; 3];
                    let mut len = 0;
                    if self.status >= 100 {
                        status[len] = b'0' + self.status / 100;
                        len += 1;
                    }
                    if self.status >= 10 {
                        status[len] = b'0' + (self.status / 10) % 10;
                        len += 1;
                    }
                    status[len] = b'0' + self.status % 10;
                    output.push(&status[..len + 1])?;
                } else {
                    let braced = source.get(i) == Some(&b'{');
                    if braced {
                        i += 1;
                    }
                    let start = i;
                    while i < source.len()
                        && (source[i].is_ascii_alphanumeric() || source[i] == b'_')
                    {
                        i += 1;
                    }
                    if i == start {
                        return Err("expected a variable name");
                    }
                    output.push(self.variable(&source[start..i]))?;
                    if braced {
                        if source.get(i) != Some(&b'}') {
                            return Err("unfinished variable");
                        }
                    } else {
                        continue;
                    }
                }
            } else {
                output.push(&[byte])?;
            }
            i += 1;
        }
        Ok(())
    }

    fn path(&self, path: &[u8], output: &mut Bytes<256>) -> Result<()> {
        if path.first() != Some(&b'/') {
            output.push(&self.cwd[..self.cwd_len])?;
            output.push(b"/")?;
        }
        output.push(path)?;
        // Normalize lexical components only. This never grants access; every
        // normalized name is still passed through a projected WASI operation.
        let mut canonical = [0u8; 256];
        let mut length = 0;
        for part in output.get().split(|byte| *byte == b'/') {
            if part.is_empty() || part == b"." {
                continue;
            }
            if part == b".." {
                while length > 0 && canonical[length - 1] != b'/' {
                    length -= 1;
                }
                length = length.saturating_sub(1);
            } else {
                if length + part.len() + 1 > canonical.len() {
                    return Err("path too long");
                }
                canonical[length] = b'/';
                length += 1;
                canonical[length..length + part.len()].copy_from_slice(part);
                length += part.len();
            }
        }
        if length == 0 {
            canonical[0] = b'/';
            length = 1;
        }
        output.len = 0;
        output.push(&canonical[..length])
    }

    #[inline(never)]
    fn execute(
        &mut self,
        tree: &Tree<'_>,
        mut current: u16,
        input: &[u8],
        output: &mut Output<'_>,
        budget: &mut u16,
        depth: u8,
    ) -> Result<Control> {
        if depth > 5 {
            return Err("shell execution nesting limit reached");
        }
        while current != NONE {
            *budget = budget
                .checked_sub(1)
                .ok_or("shell command budget exhausted")?;
            let node = tree.nodes[current as usize];
            let result = match node.kind {
                Kind::Simple => match self.simple(tree, node, input, output, budget, depth) {
                    Ok(result) => result,
                    Err(error) => {
                        report(error)?;
                        Control::Status(2)
                    }
                },
                Kind::And | Kind::Or => {
                    match self.execute(tree, node.left, input, output, budget, depth + 1)? {
                        Control::Exit(code) => return Ok(Control::Exit(code)),
                        Control::Status(status) if (status == 0) == (node.kind == Kind::And) => {
                            self.execute(tree, node.right, input, output, budget, depth + 1)?
                        }
                        result => result,
                    }
                }
                Kind::Pipe => self.pipeline(tree, node, input, output, budget, depth)?,
                Kind::If => {
                    match self.execute(tree, node.left, input, output, budget, depth + 1)? {
                        Control::Exit(code) => return Ok(Control::Exit(code)),
                        Control::Status(status) => self.execute(
                            tree,
                            if status == 0 { node.right } else { node.other },
                            input,
                            output,
                            budget,
                            depth + 1,
                        )?,
                    }
                }
                Kind::For => self.for_loop(tree, node, input, output, budget, depth)?,
                Kind::While => {
                    loop {
                        match self.execute(tree, node.left, input, output, budget, depth + 1)? {
                            Control::Exit(code) => return Ok(Control::Exit(code)),
                            Control::Status(status) if status != 0 => break,
                            _ => {}
                        }
                        if let Control::Exit(code) =
                            self.execute(tree, node.right, input, output, budget, depth + 1)?
                        {
                            return Ok(Control::Exit(code));
                        }
                    }
                    Control::Status(0)
                }
            };
            match result {
                Control::Exit(code) => return Ok(Control::Exit(code)),
                Control::Status(code) => self.status = code,
            }
            current = node.next;
        }
        Ok(Control::Status(self.status))
    }

    #[inline(never)]
    fn pipeline(
        &mut self,
        tree: &Tree<'_>,
        node: Node,
        input: &[u8],
        output: &mut Output<'_>,
        budget: &mut u16,
        depth: u8,
    ) -> Result<Control> {
        let mut pipe = Bytes::new();
        if let Control::Exit(code) = self.execute(
            tree,
            node.left,
            input,
            &mut Output::Buffer(&mut pipe),
            budget,
            depth + 1,
        )? {
            return Ok(Control::Exit(code));
        }
        self.execute(tree, node.right, pipe.get(), output, budget, depth + 1)
    }

    #[inline(never)]
    fn for_loop(
        &mut self,
        tree: &Tree<'_>,
        node: Node,
        input: &[u8],
        output: &mut Output<'_>,
        budget: &mut u16,
        depth: u8,
    ) -> Result<Control> {
        let name = tree.tokens[node.start as usize].bytes(tree.source);
        let mut values = Arguments::new();
        for token in &tree.tokens[node.start as usize + 2..node.end as usize] {
            let mut value = Bytes::<512>::new();
            self.expand(token.bytes(tree.source), &mut value)?;
            values.push(value.get())?;
        }
        self.status = 0;
        for index in 0..values.len {
            self.assign(name, values.get(index))?;
            if let Control::Exit(code) =
                self.execute(tree, node.left, input, output, budget, depth + 1)?
            {
                return Ok(Control::Exit(code));
            }
        }
        Ok(Control::Status(self.status))
    }

    #[inline(never)]
    fn simple(
        &mut self,
        tree: &Tree<'_>,
        node: Node,
        input: &[u8],
        output: &mut Output<'_>,
        budget: &mut u16,
        depth: u8,
    ) -> Result<Control> {
        let mut args = Arguments::new();
        let mut read_path = Bytes::<256>::new();
        let mut write_path = Bytes::<256>::new();
        let mut write_kind = TokenKind::End;
        let mut index = node.start as usize;
        while index < node.end as usize {
            let token = tree.tokens[index];
            let mut value = Bytes::<512>::new();
            if token.kind == TokenKind::Word {
                self.expand(token.bytes(tree.source), &mut value)?;
                if args.len == 0
                    && let Some(equal) = value.get().iter().position(|byte| *byte == b'=')
                {
                    self.assign(&value.get()[..equal], &value.get()[equal + 1..])?;
                } else {
                    args.push(value.get())?;
                }
            } else {
                index += 1;
                self.expand(tree.tokens[index].bytes(tree.source), &mut value)?;
                if token.kind == TokenKind::Read {
                    if read_path.len != 0 {
                        return Err("duplicate input redirect");
                    }
                    self.path(value.get(), &mut read_path)?;
                } else {
                    if write_path.len != 0 {
                        return Err("duplicate output redirect");
                    }
                    self.path(value.get(), &mut write_path)?;
                    write_kind = token.kind;
                }
            }
            index += 1;
        }
        let mut redirected = Bytes::new();
        let input = if read_path.len != 0 {
            read_file(read_path.get(), &mut redirected)?;
            redirected.get()
        } else {
            input
        };
        // Open the real destination before executing the command. Close is an
        // explicit fallible operation, including the shared service boundary.
        let fd = if write_path.len != 0 {
            Some(wasi::open(
                write_path.get(),
                wasi::WRITE,
                if write_kind == TokenKind::Append {
                    1
                } else {
                    9
                },
                if write_kind == TokenKind::Append {
                    1
                } else {
                    0
                },
            )?)
        } else {
            None
        };
        let result = if let Some(fd) = fd {
            self.command(&args, input, &mut Output::Fd(fd), budget, depth)
        } else {
            self.command(&args, input, output, budget, depth)
        };
        let closed = if let Some(fd) = fd {
            wasi::close(fd)
        } else {
            Ok(())
        };
        closed?;
        result
    }

    #[inline(never)]
    fn command(
        &mut self,
        args: &Arguments,
        input: &[u8],
        output: &mut Output<'_>,
        budget: &mut u16,
        depth: u8,
    ) -> Result<Control> {
        let name = args.get(0);
        let status = match name {
            b"" | b":" | b"true" => 0,
            b"false" => 1,
            b"echo" => {
                let start = if args.get(1) == b"-n" { 2 } else { 1 };
                for index in start..args.len {
                    if index > start {
                        output.write(b" ")?;
                    }
                    output.write(args.get(index))?;
                }
                if start == 1 {
                    output.write(b"\n")?;
                }
                0
            }
            b"printf" => {
                printf(args, output)?;
                0
            }
            b"cat" => {
                if args.len == 1 {
                    output.write(input)?;
                }
                for index in 1..args.len {
                    let mut path = Bytes::<256>::new();
                    self.path(args.get(index), &mut path)?;
                    copy_file(path.get(), output)?;
                }
                0
            }
            b"cp" => {
                self.copy(args)?;
                0
            }
            b"pwd" => {
                output.write(&self.cwd[..self.cwd_len])?;
                output.write(b"\n")?;
                0
            }
            b"cd" => {
                let mut path = Bytes::<256>::new();
                self.path(if args.len == 1 { b"/" } else { args.get(1) }, &mut path)?;
                let fd = wasi::open(path.get(), wasi::READDIR, 2, 4)?;
                wasi::close(fd)?;
                self.cwd[..path.len].copy_from_slice(path.get());
                self.cwd_len = path.len;
                0
            }
            b"ls" => {
                let mut path = Bytes::<256>::new();
                self.path(if args.len == 1 { b"." } else { args.get(1) }, &mut path)?;
                list(path.get(), output)?;
                0
            }
            b"grep" => grep(args, input, output)?,
            b"wc" => {
                number(input.len() as i64, output)?;
                output.write(b"\n")?;
                0
            }
            b"expr" => {
                number(expression(args)?, output)?;
                output.write(b"\n")?;
                0
            }
            b"read" => {
                if args.len != 2 {
                    return Err("read VARIABLE consumes pipeline or redirected input");
                }
                let line = input.split(|byte| *byte == b'\n').next().unwrap_or(b"");
                self.assign(args.get(1), line)?;
                u8::from(input.is_empty())
            }
            b"test" | b"[" => test(args)?,
            b"." | b"source" => return self.source(args.get(1), input, output, budget, depth),
            b"exit" => {
                return Ok(Control::Exit(if args.len == 1 {
                    self.status
                } else {
                    integer(args.get(1))?
                        .try_into()
                        .map_err(|_| "exit status must be 0..255")?
                }));
            }
            b"help" => {
                output.write(b"echo printf cat cp ls pwd cd grep wc expr read test [ true false : source . exit\nquotes $variables $? ; && || | < > >> if for while\n")?;
                0
            }
            _ => {
                wasi::write(2, b"wash: command not found: ")?;
                wasi::write(2, name)?;
                wasi::write(2, b"\n")?;
                127
            }
        };
        Ok(Control::Status(status))
    }
    #[inline(never)]
    fn copy(&self, args: &Arguments) -> Result<()> {
        if args.len != 3 {
            return Err("cp SOURCE DESTINATION");
        }
        let mut path = Bytes::<256>::new();
        self.path(args.get(1), &mut path)?;
        let mut bytes = Bytes::new();
        read_file(path.get(), &mut bytes)?;
        path.len = 0;
        self.path(args.get(2), &mut path)?;
        let fd = wasi::open(path.get(), wasi::WRITE, 9, 0)?;
        let written = wasi::write(fd, bytes.get());
        let closed = wasi::close(fd);
        written?;
        closed?;
        Ok(())
    }
    #[inline(never)]
    fn source(
        &mut self,
        name: &[u8],
        input: &[u8],
        output: &mut Output<'_>,
        budget: &mut u16,
        depth: u8,
    ) -> Result<Control> {
        let mut path = Bytes::<256>::new();
        self.path(name, &mut path)?;
        let mut source = Bytes::<BYTES>::new();
        read_file(path.get(), &mut source)?;
        let tree = Tree::parse(source.get())?;
        self.execute(&tree, tree.root, input, output, budget, depth + 1)
    }
}

#[inline(never)]
fn copy_file(path: &[u8], output: &mut Output<'_>) -> Result<()> {
    let fd = wasi::open(path, wasi::READ, 0, 0)?;
    let result = (|| {
        let mut bytes = [0; 96];
        loop {
            let used = wasi::read(fd, &mut bytes)?;
            if used == 0 {
                return Ok(());
            }
            output.write(&bytes[..used])?;
        }
    })();
    let closed = wasi::close(fd);
    result?;
    closed
}
fn read_file(path: &[u8], bytes: &mut Bytes) -> Result<()> {
    copy_file(path, &mut Output::Buffer(bytes))
}

#[inline(never)]
fn list(path: &[u8], output: &mut Output<'_>) -> Result<()> {
    let fd = wasi::open(path, wasi::READDIR, 2, 4)?;
    let result = (|| {
        let mut cookie = 0;
        loop {
            let mut bytes = [0; 96];
            let used = wasi::readdir(fd, &mut bytes, cookie)?;
            let mut offset = 0;
            while offset + 24 <= used {
                let length = u32::from_le_bytes(
                    bytes[offset + 16..offset + 20]
                        .try_into()
                        .map_err(|_| "invalid directory entry")?,
                ) as usize;
                if offset + 24 + length > used {
                    break;
                }
                cookie = u64::from_le_bytes(
                    bytes[offset..offset + 8]
                        .try_into()
                        .map_err(|_| "invalid directory cookie")?,
                );
                output.write(&bytes[offset + 24..offset + 24 + length])?;
                output.write(b"\n")?;
                offset += 24 + length;
            }
            if used < 96 {
                return Ok(());
            }
            if offset == 0 {
                return Err("directory name exceeds WASI chunk");
            }
        }
    })();
    let closed = wasi::close(fd);
    result?;
    closed
}

fn integer(bytes: &[u8]) -> Result<i64> {
    let (sign, bytes) = if bytes.first() == Some(&b'-') {
        (-1i64, &bytes[1..])
    } else {
        (1, bytes)
    };
    if bytes.is_empty() {
        return Err("expected an integer");
    }
    let mut value = 0i64;
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return Err("expected an integer");
        }
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add((byte - b'0') as i64))
            .ok_or("integer overflow")?;
    }
    value.checked_mul(sign).ok_or("integer overflow")
}
fn number(value: i64, output: &mut Output<'_>) -> Result<()> {
    let mut bytes = [0; 21];
    let mut end = bytes.len();
    let mut rest = value.unsigned_abs();
    loop {
        end -= 1;
        bytes[end] = b'0' + (rest % 10) as u8;
        rest /= 10;
        if rest == 0 {
            break;
        }
    }
    if value < 0 {
        end -= 1;
        bytes[end] = b'-';
    }
    output.write(&bytes[end..])
}
fn expression(args: &Arguments) -> Result<i64> {
    if args.len != 4 {
        return Err("expr INTEGER OP INTEGER");
    }
    let left = integer(args.get(1))?;
    let right = integer(args.get(3))?;
    match args.get(2) {
        b"+" => left.checked_add(right),
        b"-" => left.checked_sub(right),
        b"*" => left.checked_mul(right),
        b"/" => left.checked_div(right),
        b"%" => left.checked_rem(right),
        _ => return Err("unknown arithmetic operator"),
    }
    .ok_or("arithmetic overflow or division by zero")
}
fn test(args: &Arguments) -> Result<u8> {
    let end = if args.get(0) == b"[" {
        if args.get(args.len - 1) != b"]" {
            return Err("missing ]");
        }
        args.len - 1
    } else {
        args.len
    };
    let yes = match end {
        1 => false,
        2 => !args.get(1).is_empty(),
        3 => match args.get(1) {
            b"-n" => !args.get(2).is_empty(),
            b"-z" => args.get(2).is_empty(),
            _ => return Err("test -n/-z STRING"),
        },
        4 => match args.get(2) {
            b"=" => args.get(1) == args.get(3),
            b"!=" => args.get(1) != args.get(3),
            b"-eq" => integer(args.get(1))? == integer(args.get(3))?,
            b"-ne" => integer(args.get(1))? != integer(args.get(3))?,
            b"-lt" => integer(args.get(1))? < integer(args.get(3))?,
            b"-le" => integer(args.get(1))? <= integer(args.get(3))?,
            b"-gt" => integer(args.get(1))? > integer(args.get(3))?,
            b"-ge" => integer(args.get(1))? >= integer(args.get(3))?,
            _ => return Err("unsupported test operator"),
        },
        _ => return Err("invalid test arguments"),
    };
    Ok(u8::from(!yes))
}
fn grep(args: &Arguments, input: &[u8], output: &mut Output<'_>) -> Result<u8> {
    if args.len != 2 || args.get(1).is_empty() {
        return Err("grep NONEMPTY_LITERAL reads pipeline or redirected input");
    }
    let needle = args.get(1);
    let mut found = false;
    for line in input.split_inclusive(|byte| *byte == b'\n') {
        if line.windows(needle.len()).any(|part| part == needle) {
            output.write(line)?;
            found = true;
        }
    }
    Ok(u8::from(!found))
}
#[inline(never)]
fn printf(args: &Arguments, output: &mut Output<'_>) -> Result<()> {
    let format = args.get(1);
    let mut argument = 2;
    loop {
        let before = argument;
        let mut i = 0;
        while i < format.len() {
            if format[i] == b'\\' {
                i += 1;
                let byte = match format.get(i) {
                    Some(b'n') => b'\n',
                    Some(b't') => b'\t',
                    Some(b'r') => b'\r',
                    Some(b'\\') => b'\\',
                    _ => return Err("unsupported printf escape"),
                };
                output.write(&[byte])?;
            } else if format[i] == b'%' {
                i += 1;
                match format.get(i) {
                    Some(b'%') => output.write(b"%")?,
                    Some(b's') => {
                        output.write(args.get(argument))?;
                        argument += 1;
                    }
                    Some(b'd') => {
                        number(
                            if argument < args.len {
                                integer(args.get(argument))?
                            } else {
                                0
                            },
                            output,
                        )?;
                        argument += 1;
                    }
                    _ => return Err("printf supports %s %d %%"),
                }
            } else {
                output.write(&format[i..i + 1])?;
            }
            i += 1;
        }
        if argument == before || argument >= args.len {
            return Ok(());
        }
    }
}

#[inline(never)]
pub fn run() -> u32 {
    let mut shell = Shell::new();
    if wasi::write(1, b"hibana-wash\nwash> ").is_err() {
        return 1;
    }
    loop {
        let mut length = [0; 4];
        if wasi::read_exact(0, &mut length).is_err() {
            return 1;
        }
        let length = u32::from_le_bytes(length) as usize;
        if length > BYTES {
            let _ = wasi::write(2, b"wash: input exceeds 4096 bytes\n");
            return 2;
        }
        let mut source = Bytes::<BYTES>::new();
        source.len = length;
        if wasi::read_exact(0, &mut source.bytes[..length]).is_err() {
            return 1;
        }
        let result = match Tree::parse(source.get()) {
            Ok(tree) => shell.execute(&tree, tree.root, b"", &mut Output::Fd(1), &mut 1024, 0),
            Err(error) => Err(error),
        };
        match result {
            Ok(Control::Exit(code)) => return code as u32,
            Ok(Control::Status(code)) => shell.status = code,
            Err(error) => {
                let _ = report(error);
                shell.status = 2;
            }
        }
        if wasi::write(1, b"wash> ").is_err() {
            return 1;
        }
    }
}

fn report(error: &str) -> Result<()> {
    wasi::write(2, b"wash: ")?;
    wasi::write(2, error.as_bytes())?;
    wasi::write(2, b"\n")
}
