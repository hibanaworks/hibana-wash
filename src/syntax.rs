//! Shell syntax only. These nodes carry no task identity or permission.

pub const NONE: u16 = u16::MAX;
const TOKENS: usize = 256;
const NODES: usize = 128;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TokenKind {
    End,
    Word,
    Sep,
    And,
    Or,
    Pipe,
    Read,
    Write,
    Append,
}

#[derive(Clone, Copy)]
pub struct Token {
    pub kind: TokenKind,
    start: u16,
    end: u16,
}
impl Token {
    const EMPTY: Self = Self {
        kind: TokenKind::End,
        start: 0,
        end: 0,
    };
    pub fn bytes<'a>(&self, source: &'a [u8]) -> &'a [u8] {
        &source[self.start as usize..self.end as usize]
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Simple,
    And,
    Or,
    Pipe,
    If,
    For,
    While,
}

#[derive(Clone, Copy)]
#[cfg_attr(not(target_os = "wasi"), allow(dead_code))]
pub struct Node {
    pub kind: Kind,
    pub start: u16,
    pub end: u16,
    pub left: u16,
    pub right: u16,
    pub other: u16,
    pub next: u16,
}
impl Node {
    const EMPTY: Self = Self {
        kind: Kind::Simple,
        start: 0,
        end: 0,
        left: NONE,
        right: NONE,
        other: NONE,
        next: NONE,
    };
}

pub struct Tree<'a> {
    pub source: &'a [u8],
    pub tokens: [Token; TOKENS],
    pub nodes: [Node; NODES],
    pub root: u16,
    token_count: usize,
    node_count: usize,
    position: usize,
}

impl<'a> Tree<'a> {
    #[inline(never)]
    pub fn parse(source: &'a [u8]) -> Result<Self, &'static str> {
        if source.len() > u16::MAX as usize {
            return Err("script too large");
        }
        let mut tree = Self {
            source,
            tokens: [Token::EMPTY; TOKENS],
            nodes: [Node::EMPTY; NODES],
            root: NONE,
            token_count: 0,
            node_count: 0,
            position: 0,
        };
        tree.lex()?;
        tree.root = tree.list(&[], 0)?;
        if tree.position != tree.token_count {
            return Err("unexpected shell token");
        }
        Ok(tree)
    }

    fn token(&mut self, kind: TokenKind, start: usize, end: usize) -> Result<(), &'static str> {
        if self.token_count == TOKENS {
            return Err("too many shell tokens");
        }
        self.tokens[self.token_count] = Token {
            kind,
            start: start as u16,
            end: end as u16,
        };
        self.token_count += 1;
        Ok(())
    }

    #[inline(never)]
    fn lex(&mut self) -> Result<(), &'static str> {
        let mut i = 0;
        while i < self.source.len() {
            let start = i;
            match self.source[i] {
                0 => return Err("NUL is not shell text"),
                b' ' | b'\t' | b'\r' => {
                    i += 1;
                    continue;
                }
                b'#' => {
                    while i < self.source.len() && self.source[i] != b'\n' {
                        i += 1;
                    }
                    continue;
                }
                b';' | b'\n' => {
                    i += 1;
                    self.token(TokenKind::Sep, start, i)?;
                }
                b'&' => {
                    if self.source.get(i + 1) != Some(&b'&') {
                        return Err("background jobs are not supported");
                    }
                    i += 2;
                    self.token(TokenKind::And, start, i)?;
                }
                b'|' => {
                    i += 1;
                    let kind = if self.source.get(i) == Some(&b'|') {
                        i += 1;
                        TokenKind::Or
                    } else {
                        TokenKind::Pipe
                    };
                    self.token(kind, start, i)?;
                }
                b'<' => {
                    i += 1;
                    self.token(TokenKind::Read, start, i)?;
                }
                b'>' => {
                    i += 1;
                    let kind = if self.source.get(i) == Some(&b'>') {
                        i += 1;
                        TokenKind::Append
                    } else {
                        TokenKind::Write
                    };
                    self.token(kind, start, i)?;
                }
                _ => {
                    let mut quote = 0;
                    while i < self.source.len() {
                        let byte = self.source[i];
                        if byte == 0 {
                            return Err("NUL is not shell text");
                        }
                        if byte == b'\\' && quote != b'\'' {
                            i += 1;
                            if i == self.source.len() {
                                return Err("unfinished escape");
                            }
                            i += 1;
                            continue;
                        }
                        if quote != 0 {
                            if byte == quote {
                                quote = 0;
                            }
                        } else if byte == b'\'' || byte == b'"' {
                            quote = byte;
                        } else if b" \t\r\n;&|<>".contains(&byte) {
                            break;
                        }
                        i += 1;
                    }
                    if quote != 0 {
                        return Err("unfinished quote");
                    }
                    self.token(TokenKind::Word, start, i)?;
                }
            }
        }
        Ok(())
    }

    fn kind(&self) -> TokenKind {
        if self.position == self.token_count {
            TokenKind::End
        } else {
            self.tokens[self.position].kind
        }
    }
    fn word(&self, expected: &[u8]) -> bool {
        self.kind() == TokenKind::Word && self.tokens[self.position].bytes(self.source) == expected
    }
    fn take_word(&mut self, expected: &[u8]) -> Result<(), &'static str> {
        if !self.word(expected) {
            return Err("missing shell keyword");
        }
        self.position += 1;
        Ok(())
    }
    fn node(&mut self, node: Node) -> Result<u16, &'static str> {
        if self.node_count == NODES {
            return Err("too many shell commands");
        }
        let id = self.node_count;
        self.nodes[id] = node;
        self.node_count += 1;
        Ok(id as u16)
    }
    fn separators(&mut self) {
        while self.kind() == TokenKind::Sep {
            self.position += 1;
        }
    }

    #[inline(never)]
    fn list(&mut self, stops: &[&[u8]], depth: u8) -> Result<u16, &'static str> {
        if depth > 4 {
            return Err("shell nesting limit reached");
        }
        let mut head = NONE;
        let mut tail = NONE;
        self.separators();
        while self.kind() != TokenKind::End && !stops.iter().any(|stop| self.word(stop)) {
            let item = self.and_or(depth)?;
            if head == NONE {
                head = item;
            } else {
                self.nodes[tail as usize].next = item;
            }
            tail = item;
            if self.kind() == TokenKind::End {
                break;
            }
            if self.kind() != TokenKind::Sep {
                return Err("commands need a separator");
            }
            self.separators();
        }
        Ok(head)
    }

    #[inline(never)]
    fn and_or(&mut self, depth: u8) -> Result<u16, &'static str> {
        let mut left = self.pipeline(depth)?;
        while self.kind() == TokenKind::And || self.kind() == TokenKind::Or {
            let kind = if self.kind() == TokenKind::And {
                Kind::And
            } else {
                Kind::Or
            };
            self.position += 1;
            let right = self.pipeline(depth)?;
            left = self.node(Node {
                kind,
                left,
                right,
                ..Node::EMPTY
            })?;
        }
        Ok(left)
    }

    #[inline(never)]
    fn pipeline(&mut self, depth: u8) -> Result<u16, &'static str> {
        let mut left = self.command(depth)?;
        while self.kind() == TokenKind::Pipe {
            self.position += 1;
            let right = self.command(depth)?;
            left = self.node(Node {
                kind: Kind::Pipe,
                left,
                right,
                ..Node::EMPTY
            })?;
        }
        Ok(left)
    }

    #[inline(never)]
    fn command(&mut self, depth: u8) -> Result<u16, &'static str> {
        if self.word(b"if") || self.word(b"while") {
            let kind = if self.word(b"if") {
                Kind::If
            } else {
                Kind::While
            };
            self.position += 1;
            let left = self.list(&[if kind == Kind::If { b"then" } else { b"do" }], depth + 1)?;
            self.take_word(if kind == Kind::If { b"then" } else { b"do" })?;
            let right = self.list(
                if kind == Kind::If {
                    &[b"else", b"fi"]
                } else {
                    &[b"done"]
                },
                depth + 1,
            )?;
            let other = if kind == Kind::If && self.word(b"else") {
                self.position += 1;
                self.list(&[b"fi"], depth + 1)?
            } else {
                NONE
            };
            self.take_word(if kind == Kind::If { b"fi" } else { b"done" })?;
            if left == NONE || right == NONE {
                return Err("empty shell condition or body");
            }
            return self.node(Node {
                kind,
                left,
                right,
                other,
                ..Node::EMPTY
            });
        }
        if self.word(b"for") {
            self.position += 1;
            if self.kind() != TokenKind::Word
                || !name(self.tokens[self.position].bytes(self.source))
            {
                return Err("invalid loop variable");
            }
            let start = self.position as u16;
            self.position += 1;
            self.take_word(b"in")?;
            while self.kind() == TokenKind::Word {
                self.position += 1;
            }
            let end = self.position as u16;
            if self.kind() != TokenKind::Sep {
                return Err("for needs a separator before do");
            }
            self.separators();
            self.take_word(b"do")?;
            let left = self.list(&[b"done"], depth + 1)?;
            self.take_word(b"done")?;
            if left == NONE {
                return Err("empty loop body");
            }
            return self.node(Node {
                kind: Kind::For,
                start,
                end,
                left,
                ..Node::EMPTY
            });
        }
        for reserved in [b"then".as_slice(), b"else", b"fi", b"do", b"done"] {
            if self.word(reserved) {
                return Err("unexpected shell keyword");
            }
        }
        let start = self.position as u16;
        loop {
            match self.kind() {
                TokenKind::Word => self.position += 1,
                TokenKind::Read | TokenKind::Write | TokenKind::Append => {
                    self.position += 1;
                    if self.kind() != TokenKind::Word {
                        return Err("redirection needs a path");
                    }
                    self.position += 1;
                }
                _ => break,
            }
        }
        if self.position as u16 == start {
            return Err("expected a command");
        }
        self.node(Node {
            start,
            end: self.position as u16,
            ..Node::EMPTY
        })
    }
}

pub fn name(bytes: &[u8]) -> bool {
    !bytes.is_empty()
        && (bytes[0].is_ascii_alphabetic() || bytes[0] == b'_')
        && bytes
            .iter()
            .all(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_scripts_and_preserves_quoted_json() {
        let source = br#"value='{"component":"application"}';
for path in /work/a /work/b; do
    printf '%s\n' "$value" > "$path"
done
if false || true; then cat /work/a | grep application; else echo no; fi"#;
        let tree = Tree::parse(source).unwrap();
        assert_eq!(
            tree.tokens[0].bytes(source),
            br#"value='{"component":"application"}'"#
        );
        assert_eq!(
            tree.nodes[tree.root as usize].kind as u8,
            Kind::Simple as u8
        );
        let loop_node = tree.nodes[tree.nodes[tree.root as usize].next as usize];
        assert!(loop_node.kind == Kind::For);
    }

    #[test]
    fn rejects_incomplete_programs_before_execution() {
        for text in [
            "echo ok; if true; then echo partial",
            "echo x >",
            "echo 'bad",
            "echo x &&",
            "for x in a; do; done",
            "echo x & echo y",
            "echo x\0",
        ] {
            assert!(Tree::parse(text.as_bytes()).is_err(), "{text}");
        }
    }

    #[test]
    fn long_command_lists_are_linked_without_nested_sequence_nodes() {
        let tree = Tree::parse(b"echo a; echo b; echo c; echo d").unwrap();
        let mut current = tree.root;
        let mut count = 0;
        while current != NONE {
            assert!(tree.nodes[current as usize].kind == Kind::Simple);
            current = tree.nodes[current as usize].next;
            count += 1;
        }
        assert_eq!(count, 4);
    }
}
