use crate::{
    files,
    syntax::{self, Command, Kind, Token},
};
use std::{
    borrow::Cow,
    collections::BTreeMap,
    error::Error,
    fs::{self, OpenOptions},
    io::{self, Read, Write},
    path::{Component, PathBuf},
};

type Result<T> = std::result::Result<T, Box<dyn Error>>;
const SCRIPT_BYTES: usize = 4096;

#[derive(Clone, Copy)]
enum Control {
    Status(i32),
    Exit(i32),
}

struct Shell {
    variables: BTreeMap<String, String>,
    cwd: PathBuf,
    status: i32,
}

impl Shell {
    fn new() -> Self {
        Self {
            variables: BTreeMap::new(),
            cwd: PathBuf::from("/"),
            status: 0,
        }
    }

    fn path(&self, path: &str) -> PathBuf {
        let joined = self.cwd.join(path);
        let mut result = PathBuf::from("/");
        for part in joined.components() {
            match part {
                Component::Normal(name) => result.push(name),
                Component::ParentDir => {
                    result.pop();
                }
                _ => {}
            }
        }
        result
    }

    fn assign(&mut self, name: &str, value: String) -> Result<()> {
        if !syntax::name(name) {
            return Err("invalid variable name".into());
        }
        self.variables.insert(name.to_owned(), value);
        Ok(())
    }

    fn expand(&self, word: &str) -> Result<String> {
        let mut output = String::new();
        let mut chars = word.chars().peekable();
        let mut quote = None;
        while let Some(ch) = chars.next() {
            match ch {
                '\'' | '"' if quote == Some(ch) => quote = None,
                '\'' | '"' if quote.is_none() => quote = Some(ch),
                '\\' if quote != Some('\'') => {
                    let escaped = chars.next().ok_or("unfinished escape")?;
                    if escaped != '\n' {
                        output.push(escaped);
                    }
                }
                '$' if quote != Some('\'') => {
                    if chars.peek() == Some(&'?') {
                        chars.next();
                        output.push_str(&self.status.to_string());
                        continue;
                    }
                    let braced = chars.peek() == Some(&'{');
                    if braced {
                        chars.next();
                    }
                    let name: String = std::iter::from_fn(|| {
                        chars.next_if(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
                    })
                    .collect();
                    if name.is_empty() {
                        return Err("expected a variable name".into());
                    }
                    if braced && chars.next() != Some('}') {
                        return Err("unfinished variable".into());
                    }
                    if name == "PWD" {
                        output.push_str(&self.cwd.to_string_lossy());
                    } else if let Some(value) = self.variables.get(&name) {
                        output.push_str(value);
                    }
                }
                _ => output.push(ch),
            }
        }
        Ok(output)
    }

    fn list(
        &mut self,
        commands: &[Command<'_>],
        input: &[u8],
        output: &mut dyn Write,
        budget: &mut usize,
    ) -> Result<Control> {
        let mut last = 0;
        for command in commands {
            match self.execute(command, input, output, budget)? {
                Control::Exit(status) => return Ok(Control::Exit(status)),
                Control::Status(status) => {
                    self.status = status;
                    last = status;
                }
            }
        }
        Ok(Control::Status(last))
    }

    fn execute(
        &mut self,
        command: &Command<'_>,
        input: &[u8],
        output: &mut dyn Write,
        budget: &mut usize,
    ) -> Result<Control> {
        *budget = budget
            .checked_sub(1)
            .ok_or("shell command budget exhausted")?;
        let result = match command {
            Command::Simple(words) => match self.simple(words, input, output, budget) {
                Ok(result) => result,
                Err(error) => {
                    eprintln!("wash: {error}");
                    Control::Status(2)
                }
            },
            Command::And(left, right) | Command::Or(left, right) => {
                let left = self.execute(left, input, output, budget)?;
                match left {
                    Control::Exit(_) => left,
                    Control::Status(status)
                        if (status == 0) == matches!(command, Command::And(..)) =>
                    {
                        self.execute(right, input, output, budget)?
                    }
                    _ => left,
                }
            }
            Command::Pipeline(commands) => {
                let mut bytes = Cow::Borrowed(input);
                let mut last = Control::Status(0);
                for (index, command) in commands.iter().enumerate() {
                    if index + 1 == commands.len() {
                        last = self.execute(command, &bytes, output, budget)?;
                    } else {
                        let mut pipe = Vec::new();
                        last = self.execute(command, &bytes, &mut pipe, budget)?;
                        bytes = Cow::Owned(pipe);
                    }
                    if matches!(last, Control::Exit(_)) {
                        return Ok(last);
                    }
                }
                last
            }
            Command::If {
                condition,
                then_body,
                else_body,
            } => match self.list(condition, input, output, budget)? {
                Control::Exit(status) => Control::Exit(status),
                Control::Status(status) => self.list(
                    if status == 0 { then_body } else { else_body },
                    input,
                    output,
                    budget,
                )?,
            },
            Command::For { name, values, body } => {
                let values = values
                    .iter()
                    .map(|word| self.expand(word))
                    .collect::<Result<Vec<_>>>()?;
                let mut last = Control::Status(0);
                for value in values {
                    self.assign(name, value)?;
                    last = self.list(body, input, output, budget)?;
                    if matches!(last, Control::Exit(_)) {
                        return Ok(last);
                    }
                }
                last
            }
            Command::While { condition, body } => {
                loop {
                    match self.list(condition, input, output, budget)? {
                        Control::Exit(status) => return Ok(Control::Exit(status)),
                        Control::Status(status) if status != 0 => break,
                        _ => {}
                    }
                    if let Control::Exit(status) = self.list(body, input, output, budget)? {
                        return Ok(Control::Exit(status));
                    }
                }
                Control::Status(0)
            }
        };
        if let Control::Status(status) = result {
            self.status = status;
        }
        Ok(result)
    }

    fn simple(
        &mut self,
        words: &[Token<'_>],
        input: &[u8],
        output: &mut dyn Write,
        budget: &mut usize,
    ) -> Result<Control> {
        let mut args = Vec::new();
        let mut input_path = None;
        let mut output_path = None;
        let mut words = words.iter();
        while let Some(token) = words.next() {
            if token.kind == Kind::Word {
                let word = self.expand(token.text)?;
                if args.is_empty()
                    && let Some((name, value)) = word.split_once('=')
                {
                    self.assign(name, value.to_owned())?;
                } else {
                    args.push(word);
                }
            } else {
                let path =
                    self.path(&self.expand(words.next().ok_or("missing redirect path")?.text)?);
                if token.kind == Kind::Read {
                    if input_path.replace(path).is_some() {
                        return Err("duplicate input redirect".into());
                    }
                } else if output_path
                    .replace((path, token.kind == Kind::Append))
                    .is_some()
                {
                    return Err("duplicate output redirect".into());
                }
            }
        }
        let bytes;
        let input = if let Some(path) = input_path {
            bytes = fs::read(path)?;
            bytes.as_slice()
        } else {
            input
        };
        if let Some((path, append)) = output_path {
            let mut file = OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(!append)
                .append(append)
                .open(path)?;
            let result = self.command(&args, input, &mut file, budget);
            let closed = files::close(file);
            closed?;
            result
        } else {
            self.command(&args, input, output, budget)
        }
    }

    fn command(
        &mut self,
        args: &[String],
        input: &[u8],
        output: &mut dyn Write,
        budget: &mut usize,
    ) -> Result<Control> {
        let Some(command) = args.first().map(String::as_str) else {
            return Ok(Control::Status(0));
        };
        let arg = |index| args.get(index).map(String::as_str).unwrap_or("");
        let status = match command {
            ":" | "true" => 0,
            "false" => 1,
            "echo" => {
                let start = if arg(1) == "-n" { 2 } else { 1 };
                for (index, word) in args.iter().enumerate().skip(start) {
                    if index > start {
                        write!(output, " ")?;
                    }
                    write!(output, "{word}")?;
                }
                if start == 1 {
                    writeln!(output)?;
                }
                0
            }
            "printf" => {
                printf(&args[1..], output)?;
                0
            }
            "cat" => {
                if args.len() == 1 {
                    output.write_all(input)?;
                }
                for path in &args[1..] {
                    output.write_all(&fs::read(self.path(path))?)?;
                }
                0
            }
            "cp" => {
                if args.len() != 3 {
                    return Err("cp SOURCE DESTINATION".into());
                }
                let bytes = fs::read(self.path(arg(1)))?;
                let mut file = OpenOptions::new()
                    .write(true)
                    .create(true)
                    .truncate(true)
                    .open(self.path(arg(2)))?;
                let written = file.write_all(&bytes);
                let closed = files::close(file);
                written?;
                closed?;
                0
            }
            "pwd" => {
                writeln!(output, "{}", self.cwd.display())?;
                0
            }
            "cd" => {
                let path = self.path(if args.len() == 1 { "/" } else { arg(1) });
                if !fs::metadata(&path)?.is_dir() {
                    return Err("not a directory".into());
                }
                self.cwd = path;
                0
            }
            "ls" => {
                for name in files::read_dir(&self.path(if args.len() == 1 { "." } else { arg(1) }))?
                {
                    writeln!(output, "{name}")?;
                }
                0
            }
            "grep" => {
                if args.len() != 2 || arg(1).is_empty() {
                    return Err("grep NONEMPTY_LITERAL reads pipeline or redirected input".into());
                }
                let needle = arg(1).as_bytes();
                let mut found = false;
                for line in input.split_inclusive(|byte| *byte == b'\n') {
                    if line.windows(needle.len()).any(|part| part == needle) {
                        output.write_all(line)?;
                        found = true;
                    }
                }
                i32::from(!found)
            }
            "wc" => {
                writeln!(output, "{}", input.len())?;
                0
            }
            "expr" => {
                writeln!(output, "{}", expression(&args[1..])?)?;
                0
            }
            "read" => {
                if args.len() != 2 {
                    return Err("read VARIABLE consumes pipeline or redirected input".into());
                }
                let line = input
                    .split(|byte| *byte == b'\n')
                    .next()
                    .unwrap_or_default();
                self.assign(arg(1), std::str::from_utf8(line)?.to_owned())?;
                i32::from(input.is_empty())
            }
            "test" | "[" => test(args)?,
            "." | "source" => {
                let source = fs::read_to_string(self.path(arg(1)))?;
                let commands = syntax::parse(&source)?;
                return self.list(&commands, input, output, budget);
            }
            "exit" => {
                return Ok(Control::Exit(if args.len() == 1 {
                    self.status
                } else {
                    arg(1).parse::<u8>()? as i32
                }));
            }
            "help" => {
                writeln!(
                    output,
                    "echo printf cat cp ls pwd cd grep wc expr read test [ true false : source . exit"
                )?;
                writeln!(output, "quotes $variables $? ; && || | < > >> if for while")?;
                0
            }
            _ => {
                eprintln!("wash: command not found: {command}");
                127
            }
        };
        Ok(Control::Status(status))
    }
}

fn expression(args: &[String]) -> Result<i64> {
    if args.len() != 3 {
        return Err("expr INTEGER OP INTEGER".into());
    }
    let left = args[0].parse::<i64>()?;
    let right = args[2].parse::<i64>()?;
    match args[1].as_str() {
        "+" => left.checked_add(right),
        "-" => left.checked_sub(right),
        "*" => left.checked_mul(right),
        "/" => left.checked_div(right),
        "%" => left.checked_rem(right),
        _ => return Err("unknown arithmetic operator".into()),
    }
    .ok_or_else(|| "arithmetic overflow or division by zero".into())
}

fn test(args: &[String]) -> Result<i32> {
    let args = if args[0] == "[" {
        if args.last().map(String::as_str) != Some("]") {
            return Err("missing ]".into());
        }
        &args[1..args.len() - 1]
    } else {
        &args[1..]
    };
    let yes = match args {
        [] => false,
        [value] => !value.is_empty(),
        [op, value] => match op.as_str() {
            "-n" => !value.is_empty(),
            "-z" => value.is_empty(),
            _ => return Err("test -n/-z STRING".into()),
        },
        [left, op, right] => match op.as_str() {
            "=" => left == right,
            "!=" => left != right,
            "-eq" => left.parse::<i64>()? == right.parse::<i64>()?,
            "-ne" => left.parse::<i64>()? != right.parse::<i64>()?,
            "-lt" => left.parse::<i64>()? < right.parse::<i64>()?,
            "-le" => left.parse::<i64>()? <= right.parse::<i64>()?,
            "-gt" => left.parse::<i64>()? > right.parse::<i64>()?,
            "-ge" => left.parse::<i64>()? >= right.parse::<i64>()?,
            _ => return Err("unsupported test operator".into()),
        },
        _ => return Err("invalid test arguments".into()),
    };
    Ok(i32::from(!yes))
}

fn printf(args: &[String], output: &mut dyn Write) -> Result<()> {
    let Some(format) = args.first() else {
        return Ok(());
    };
    let mut argument = 1;
    loop {
        let before = argument;
        let mut chars = format.chars();
        while let Some(ch) = chars.next() {
            match ch {
                '\\' => write!(
                    output,
                    "{}",
                    match chars.next() {
                        Some('n') => '\n',
                        Some('t') => '\t',
                        Some('r') => '\r',
                        Some('\\') => '\\',
                        _ => return Err("unsupported printf escape".into()),
                    }
                )?,
                '%' => match chars.next() {
                    Some('%') => write!(output, "%")?,
                    Some('s') => {
                        write!(
                            output,
                            "{}",
                            args.get(argument).map(String::as_str).unwrap_or("")
                        )?;
                        argument += 1;
                    }
                    Some('d') => {
                        write!(
                            output,
                            "{}",
                            args.get(argument)
                                .map(|value| value.parse::<i64>())
                                .transpose()?
                                .unwrap_or(0)
                        )?;
                        argument += 1;
                    }
                    _ => return Err("printf supports %s %d %%".into()),
                },
                _ => write!(output, "{ch}")?,
            }
        }
        if argument == before || argument >= args.len() {
            return Ok(());
        }
    }
}

pub fn run() -> i32 {
    match terminal() {
        Ok(status) => status,
        Err(error) => {
            eprintln!("wash: {error}");
            1
        }
    }
}

fn terminal() -> Result<i32> {
    let mut shell = Shell::new();
    let mut input = io::stdin().lock();
    let mut output = io::stdout().lock();
    writeln!(output, "hibana-wash")?;
    loop {
        write!(output, "wash> ")?;
        output.flush()?;
        let mut length = [0; 4];
        input.read_exact(&mut length)?;
        let length = u32::from_le_bytes(length) as usize;
        if length > SCRIPT_BYTES {
            return Err("input exceeds 4096 bytes".into());
        }
        let mut bytes = vec![0; length];
        input.read_exact(&mut bytes)?;
        let source = std::str::from_utf8(&bytes)?;
        let result = syntax::parse(source)
            .map_err(Box::<dyn Error>::from)
            .and_then(|commands| shell.list(&commands, b"", &mut output, &mut 1024));
        match result {
            Ok(Control::Exit(status)) => return Ok(status),
            Ok(Control::Status(status)) => shell.status = status,
            Err(error) => {
                eprintln!("wash: {error}");
                shell.status = 2;
            }
        }
    }
}
