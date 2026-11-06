//! Text VDF reader with byte spans, so edits touch only the affected value and
//! the rest of a file such as Steam's `config.vdf` stays byte-for-byte the same.
use std::ops::Range;

const MAX_DEPTH: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Node {
    pub key: String,
    pub body: Body,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Body {
    Value {
        value: String,
        span: Range<usize>,
    },
    Block {
        children: Vec<Node>,
        /// Byte offset of the closing brace.
        close: usize,
    },
}

impl Node {
    pub fn children(&self) -> &[Node] {
        match &self.body {
            Body::Block { children, .. } => children,
            Body::Value { .. } => &[],
        }
    }

    pub fn value(&self) -> Option<&str> {
        match &self.body {
            Body::Value { value, .. } => Some(value),
            Body::Block { .. } => None,
        }
    }
}

pub fn find<'a>(nodes: &'a [Node], key: &str) -> Option<&'a Node> {
    nodes.iter().find(|node| node.key.eq_ignore_ascii_case(key))
}

pub fn find_path<'a>(nodes: &'a [Node], path: &[&str]) -> Option<&'a Node> {
    let (first, rest) = path.split_first()?;
    let node = find(nodes, first)?;
    if rest.is_empty() {
        Some(node)
    } else {
        find_path(node.children(), rest)
    }
}

pub fn quote(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for character in value.chars() {
        match character {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

pub fn parse(text: &str) -> Result<Vec<Node>, String> {
    let mut lexer = Lexer { text, offset: 0 };
    if text.starts_with('\u{feff}') {
        lexer.offset = '\u{feff}'.len_utf8();
    }
    let (nodes, close) = parse_block(&mut lexer, 0)?;
    if close.is_some() {
        return Err("unexpected closing brace".to_owned());
    }
    Ok(nodes)
}

fn parse_block(lexer: &mut Lexer, depth: usize) -> Result<(Vec<Node>, Option<usize>), String> {
    if depth > MAX_DEPTH {
        return Err("document is nested too deeply".to_owned());
    }
    let mut nodes = Vec::new();
    loop {
        let key = match lexer.next()? {
            None if depth == 0 => return Ok((nodes, None)),
            None => return Err("missing closing brace".to_owned()),
            Some(Token::Close(position)) => return Ok((nodes, Some(position))),
            Some(Token::Open(position)) => {
                return Err(format!("unexpected opening brace at byte {position}"))
            }
            Some(Token::Text { value, .. }) => value,
        };
        let body = match lexer.next()? {
            None => return Err(format!("key \"{key}\" has no value")),
            Some(Token::Close(position)) => {
                return Err(format!("key \"{key}\" has no value at byte {position}"))
            }
            Some(Token::Text { value, span }) => {
                lexer.skip_condition();
                Body::Value { value, span }
            }
            Some(Token::Open(_)) => {
                let (children, close) = parse_block(lexer, depth + 1)?;
                let close = close.ok_or_else(|| "missing closing brace".to_owned())?;
                Body::Block { children, close }
            }
        };
        nodes.push(Node { key, body });
    }
}

enum Token {
    Text { value: String, span: Range<usize> },
    Open(usize),
    Close(usize),
}

struct Lexer<'a> {
    text: &'a str,
    offset: usize,
}

impl Lexer<'_> {
    fn skip_trivia(&mut self) {
        let bytes = self.text.as_bytes();
        loop {
            while bytes.get(self.offset).is_some_and(u8::is_ascii_whitespace) {
                self.offset += 1;
            }
            if bytes[self.offset.min(bytes.len())..].starts_with(b"//") {
                while bytes.get(self.offset).is_some_and(|byte| *byte != b'\n') {
                    self.offset += 1;
                }
            } else {
                return;
            }
        }
    }

    /// Skip a trailing platform condition such as `[$WIN32]`.
    fn skip_condition(&mut self) {
        let bytes = self.text.as_bytes();
        let mut probe = self.offset;
        while matches!(bytes.get(probe), Some(b' ' | b'\t')) {
            probe += 1;
        }
        if bytes.get(probe) == Some(&b'[') {
            while bytes
                .get(probe)
                .is_some_and(|byte| *byte != b']' && *byte != b'\n')
            {
                probe += 1;
            }
            if bytes.get(probe) == Some(&b']') {
                self.offset = probe + 1;
            }
        }
    }

    fn next(&mut self) -> Result<Option<Token>, String> {
        self.skip_trivia();
        let bytes = self.text.as_bytes();
        let start = self.offset;
        match bytes.get(start) {
            None => Ok(None),
            Some(b'{') => {
                self.offset += 1;
                Ok(Some(Token::Open(start)))
            }
            Some(b'}') => {
                self.offset += 1;
                Ok(Some(Token::Close(start)))
            }
            Some(b'"') => self.quoted(start).map(Some),
            Some(_) => {
                while bytes.get(self.offset).is_some_and(|byte| {
                    !byte.is_ascii_whitespace() && !matches!(byte, b'{' | b'}' | b'"')
                }) {
                    self.offset += 1;
                }
                Ok(Some(Token::Text {
                    value: self.text[start..self.offset].to_owned(),
                    span: start..self.offset,
                }))
            }
        }
    }

    fn quoted(&mut self, start: usize) -> Result<Token, String> {
        let mut value = String::new();
        let mut characters = self.text[start + 1..].char_indices();
        while let Some((index, character)) = characters.next() {
            match character {
                '"' => {
                    let end = start + 1 + index + 1;
                    self.offset = end;
                    return Ok(Token::Text {
                        value,
                        span: start..end,
                    });
                }
                '\\' => match characters.next() {
                    Some((_, 'n')) => value.push('\n'),
                    Some((_, 't')) => value.push('\t'),
                    Some((_, '\\')) => value.push('\\'),
                    Some((_, '"')) => value.push('"'),
                    Some((_, other)) => {
                        value.push('\\');
                        value.push(other);
                    }
                    None => break,
                },
                other => value.push(other),
            }
        }
        Err(format!("unterminated string at byte {start}"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "// comment\n\"Root\"\n{\n\t\"Path\"\t\t\"C:\\\\Games\\\\\\\"x\\\"\"\n\tBare value [$WIN32]\n\t\"Child\" { \"a\" \"1\" }\n}\n";

    #[test]
    fn parses_nested_values_with_escapes_and_case_insensitive_lookup() {
        let nodes = parse(SAMPLE).unwrap();
        assert_eq!(
            find_path(&nodes, &["root", "PATH"])
                .unwrap()
                .value()
                .unwrap(),
            "C:\\Games\\\"x\""
        );
        assert_eq!(
            find_path(&nodes, &["Root", "Bare"])
                .unwrap()
                .value()
                .unwrap(),
            "value"
        );
        assert_eq!(
            find_path(&nodes, &["Root", "Child", "a"])
                .unwrap()
                .value()
                .unwrap(),
            "1"
        );
    }

    #[test]
    fn value_spans_point_at_the_quoted_source() {
        let text = "\"a\" \"b\"";
        let nodes = parse(text).unwrap();
        let Body::Value { span, .. } = &nodes[0].body else {
            panic!("expected value");
        };
        assert_eq!(&text[span.clone()], "\"b\"");
    }

    #[test]
    fn quote_round_trips_through_the_parser() {
        let original = "a\"b\\c\td\ne 日本語";
        let text = format!("\"k\" {}", quote(original));
        assert_eq!(parse(&text).unwrap()[0].value().unwrap(), original);
    }

    #[test]
    fn malformed_documents_are_rejected() {
        for bad in [
            "\"a\"",
            "\"a\" {",
            "\"a\" { \"b\" }",
            "}",
            "{ }",
            "\"a\" \"unterminated",
            "\"a\" \"b\\",
        ] {
            assert!(parse(bad).is_err(), "accepted {bad:?}");
        }
    }

    #[test]
    fn excessive_nesting_is_rejected() {
        assert!(parse(&"\"a\" { ".repeat(10_000)).is_err());
    }
}
