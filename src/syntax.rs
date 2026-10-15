//! Shell syntax. No task identities, permissions, or coordination state.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Word,
    Separator,
    And,
    Or,
    Pipe,
    Read,
    Write,
    Append,
}

#[derive(Clone, Copy, Debug)]
pub struct Token<'a> {
    pub kind: Kind,
    pub text: &'a str,
}

#[derive(Debug)]
#[cfg_attr(not(target_os = "wasi"), allow(dead_code))]
pub enum Command<'a> {
    Simple(Vec<Token<'a>>),
    And(Box<Self>, Box<Self>),
    Or(Box<Self>, Box<Self>),
    Pipeline(Vec<Self>),
    If {
        condition: Vec<Self>,
        then_body: Vec<Self>,
        else_body: Vec<Self>,
    },
    For {
        name: &'a str,
        values: Vec<&'a str>,
        body: Vec<Self>,
    },
    While {
        condition: Vec<Self>,
        body: Vec<Self>,
    },
}

pub fn name(value: &str) -> bool {
    let mut chars = value.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

fn lex(source: &str) -> Result<Vec<Token<'_>>, &'static str> {
    let bytes = source.as_bytes();
    let mut tokens = Vec::new();
    let mut i = 0;
    while i < bytes.len() {
        let start = i;
        let kind = match bytes[i] {
            0 => return Err("NUL is not shell text"),
            b' ' | b'\t' | b'\r' => {
                i += 1;
                continue;
            }
            b'#' => {
                while i < bytes.len() && bytes[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b';' | b'\n' => {
                i += 1;
                Kind::Separator
            }
            b'&' => {
                if bytes.get(i + 1) != Some(&b'&') {
                    return Err("background jobs are not supported");
                }
                i += 2;
                Kind::And
            }
            b'|' => {
                i += 1;
                if bytes.get(i) == Some(&b'|') {
                    i += 1;
                    Kind::Or
                } else {
                    Kind::Pipe
                }
            }
            b'<' => {
                i += 1;
                Kind::Read
            }
            b'>' => {
                i += 1;
                if bytes.get(i) == Some(&b'>') {
                    i += 1;
                    Kind::Append
                } else {
                    Kind::Write
                }
            }
            _ => {
                let mut quote = None;
                while i < bytes.len() {
                    let byte = bytes[i];
                    if byte == 0 {
                        return Err("NUL is not shell text");
                    }
                    if byte == b'\\' && quote != Some(b'\'') {
                        i += 1;
                        if i == bytes.len() {
                            return Err("unfinished escape");
                        }
                        i += 1;
                        continue;
                    }
                    if let Some(delimiter) = quote {
                        if byte == delimiter {
                            quote = None;
                        }
                    } else if byte == b'\'' || byte == b'"' {
                        quote = Some(byte);
                    } else if b" \t\r\n;&|<>".contains(&byte) {
                        break;
                    }
                    i += 1;
                }
                if quote.is_some() {
                    return Err("unfinished quote");
                }
                Kind::Word
            }
        };
        tokens.push(Token {
            kind,
            text: &source[start..i],
        });
        if tokens.len() > 256 {
            return Err("too many shell tokens");
        }
    }
    Ok(tokens)
}

struct Parser<'a> {
    tokens: Vec<Token<'a>>,
    position: usize,
}
impl<'a> Parser<'a> {
    fn kind(&self) -> Option<Kind> {
        self.tokens.get(self.position).map(|token| token.kind)
    }
    fn word(&self, word: &str) -> bool {
        self.tokens
            .get(self.position)
            .is_some_and(|token| token.kind == Kind::Word && token.text == word)
    }
    fn expect(&mut self, word: &str) -> Result<(), &'static str> {
        if !self.word(word) {
            return Err("missing shell keyword");
        }
        self.position += 1;
        Ok(())
    }
    fn separators(&mut self) {
        while self.kind() == Some(Kind::Separator) {
            self.position += 1;
        }
    }

    fn list(&mut self, stops: &[&str], depth: usize) -> Result<Vec<Command<'a>>, &'static str> {
        if depth > 4 {
            return Err("shell nesting limit reached");
        }
        let mut commands = Vec::new();
        self.separators();
        while self.kind().is_some() && !stops.iter().any(|stop| self.word(stop)) {
            commands.push(self.and_or(depth)?);
            if self.kind().is_none() {
                break;
            }
            if self.kind() != Some(Kind::Separator) {
                return Err("commands need a separator");
            }
            self.separators();
        }
        Ok(commands)
    }

    fn and_or(&mut self, depth: usize) -> Result<Command<'a>, &'static str> {
        let mut command = self.pipeline(depth)?;
        while let Some(kind @ (Kind::And | Kind::Or)) = self.kind() {
            self.position += 1;
            let next = self.pipeline(depth)?;
            command = if kind == Kind::And {
                Command::And(Box::new(command), Box::new(next))
            } else {
                Command::Or(Box::new(command), Box::new(next))
            };
        }
        Ok(command)
    }

    fn pipeline(&mut self, depth: usize) -> Result<Command<'a>, &'static str> {
        let first = self.command(depth)?;
        if self.kind() != Some(Kind::Pipe) {
            return Ok(first);
        }
        let mut commands = vec![first];
        while self.kind() == Some(Kind::Pipe) {
            self.position += 1;
            commands.push(self.command(depth)?);
        }
        Ok(Command::Pipeline(commands))
    }

    fn command(&mut self, depth: usize) -> Result<Command<'a>, &'static str> {
        if self.word("if") {
            self.position += 1;
            let condition = self.list(&["then"], depth + 1)?;
            self.expect("then")?;
            let then_body = self.list(&["else", "fi"], depth + 1)?;
            let else_body = if self.word("else") {
                self.position += 1;
                self.list(&["fi"], depth + 1)?
            } else {
                vec![]
            };
            self.expect("fi")?;
            if condition.is_empty() || then_body.is_empty() {
                return Err("empty condition or body");
            }
            return Ok(Command::If {
                condition,
                then_body,
                else_body,
            });
        }
        if self.word("while") {
            self.position += 1;
            let condition = self.list(&["do"], depth + 1)?;
            self.expect("do")?;
            let body = self.list(&["done"], depth + 1)?;
            self.expect("done")?;
            if condition.is_empty() || body.is_empty() {
                return Err("empty condition or body");
            }
            return Ok(Command::While { condition, body });
        }
        if self.word("for") {
            self.position += 1;
            let token = self
                .tokens
                .get(self.position)
                .ok_or("missing loop variable")?;
            if token.kind != Kind::Word || !name(token.text) {
                return Err("invalid loop variable");
            }
            let name = token.text;
            self.position += 1;
            self.expect("in")?;
            let mut values = Vec::new();
            while self.kind() == Some(Kind::Word) {
                values.push(self.tokens[self.position].text);
                self.position += 1;
            }
            if self.kind() != Some(Kind::Separator) {
                return Err("for needs a separator before do");
            }
            self.separators();
            self.expect("do")?;
            let body = self.list(&["done"], depth + 1)?;
            self.expect("done")?;
            if body.is_empty() {
                return Err("empty loop body");
            }
            return Ok(Command::For { name, values, body });
        }
        if ["then", "else", "fi", "do", "done"]
            .iter()
            .any(|word| self.word(word))
        {
            return Err("unexpected shell keyword");
        }
        let mut words = Vec::new();
        loop {
            match self.kind() {
                Some(Kind::Word) => {
                    words.push(self.tokens[self.position]);
                    self.position += 1;
                }
                Some(Kind::Read | Kind::Write | Kind::Append) => {
                    words.push(self.tokens[self.position]);
                    self.position += 1;
                    if self.kind() != Some(Kind::Word) {
                        return Err("redirection needs a path");
                    }
                    words.push(self.tokens[self.position]);
                    self.position += 1;
                }
                _ => break,
            }
        }
        if words.is_empty() {
            return Err("expected a command");
        }
        Ok(Command::Simple(words))
    }
}

pub fn parse(source: &str) -> Result<Vec<Command<'_>>, &'static str> {
    Parser {
        tokens: lex(source)?,
        position: 0,
    }
    .list(&[], 0)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn scripts_keep_quoted_json_and_visible_structure() {
        let commands = parse(
            r#"value='{"component":"application"}';
for path in /work/a /work/b; do printf '%s\n' "$value" > "$path"; done
if false || true; then cat /work/a | grep application; else echo no; fi"#,
        )
        .unwrap();
        assert_eq!(commands.len(), 3);
        assert!(
            matches!(&commands[1], Command::For { name: "path", values, .. } if values == &["/work/a", "/work/b"])
        );
        assert!(
            matches!(&commands[2], Command::If { condition, .. } if matches!(&condition[0], Command::Or(..)))
        );
    }
    #[test]
    fn incomplete_scripts_are_rejected_before_execution() {
        for source in [
            "echo ok; if true; then echo partial",
            "echo x >",
            "echo 'bad",
            "echo x &&",
            "for x in a; do; done",
            "echo x & echo y",
            "echo x\0",
        ] {
            assert!(parse(source).is_err(), "{source}");
        }
    }
    #[test]
    fn sequential_commands_use_a_flat_list() {
        let commands = parse("echo a; echo b; echo c; echo d").unwrap();
        assert_eq!(commands.len(), 4);
        assert!(
            commands
                .iter()
                .all(|command| matches!(command, Command::Simple(_)))
        );
    }
}
