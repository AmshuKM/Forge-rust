//! The contract every Forge app implements. No dependencies, no network, no files.
//!
//! An app is one function from `Input` to `Step`. Forge runs the program once per step,
//! sends the input as one line of JSON on stdin and reads one line of JSON back.
use std::collections::BTreeMap;
use std::io::Read;

/// What Forge hands the app: the form values, and the model's reply once there is one.
pub struct Input {
    pub payload: BTreeMap<String, String>,
    pub reply: Option<String>,
}

impl Input {
    /// A form value, trimmed. Missing fields read as empty.
    pub fn get(&self, key: &str) -> &str {
        self.payload.get(key).map(|s| s.trim()).unwrap_or("")
    }
}

/// What the app tells Forge to do next.
pub enum Step {
    /// The person's input was not acceptable. The message is shown to them.
    Error(String),
    /// The app needs a model. Forge fills `prompts/<prompt>.md` with `vars`, calls the model
    /// chosen for `task`, and runs the app again with `Input::reply` set.
    Ask { task: String, prompt: String, vars: Vec<(String, String)> },
    /// The model's reply was not acceptable. Forge asks again, a limited number of times.
    Invalid(String),
    /// Done. The dashboard shows the value named "answer".
    Output(Vec<(String, String)>),
}

pub fn answer(text: impl Into<String>) -> Step {
    Step::Output(vec![("answer".to_string(), text.into())])
}

pub fn ask(task: &str, prompt: &str, vars: &[(&str, &str)]) -> Step {
    Step::Ask {
        task: task.to_string(),
        prompt: prompt.to_string(),
        vars: vars.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

/// Call this from `main`: reads the input, runs the app, prints the step.
pub fn run(app: fn(&Input) -> Step) {
    let mut text = String::new();
    let _ = std::io::stdin().read_to_string(&mut text);
    let step = match parse_input(&text) {
        Ok(input) => app(&input),
        Err(e) => Step::Error(format!("The app could not read its input: {e}")),
    };
    println!("{}", step_json(&step));
}

fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if (c as u32) < 0x20 => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn object(pairs: &[(String, String)]) -> String {
    let body: Vec<String> = pairs.iter().map(|(k, v)| format!("{}:{}", quote(k), quote(v))).collect();
    format!("{{{}}}", body.join(","))
}

/// One line of JSON, never containing a raw newline.
pub fn step_json(step: &Step) -> String {
    match step {
        Step::Error(m) => format!("{{\"error\":{}}}", quote(m)),
        Step::Invalid(m) => format!("{{\"invalid\":{}}}", quote(m)),
        Step::Output(pairs) => format!("{{\"output\":{}}}", object(pairs)),
        Step::Ask { task, prompt, vars } => {
            format!("{{\"llm\":{{\"task\":{},\"prompt\":{},\"vars\":{}}}}}", quote(task), quote(prompt), object(vars))
        }
    }
}

enum Json {
    Null,
    Text(String),
    Object(Vec<(String, Json)>),
}

struct Parser {
    chars: Vec<char>,
    at: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.at).copied()
    }

    fn space(&mut self) {
        while self.peek().is_some_and(|c| c.is_whitespace()) {
            self.at += 1;
        }
    }

    fn hex4(&mut self) -> Result<u32, String> {
        let digits: String = self.chars.iter().skip(self.at).take(4).collect();
        self.at += 4;
        u32::from_str_radix(&digits, 16).map_err(|_| "bad \\u escape".to_string())
    }

    fn string(&mut self) -> Result<String, String> {
        if self.peek() != Some('"') {
            return Err("expected a string".to_string());
        }
        self.at += 1;
        let mut out = String::new();
        loop {
            let c = self.peek().ok_or("unfinished string")?;
            self.at += 1;
            match c {
                '"' => return Ok(out),
                '\\' => {
                    let e = self.peek().ok_or("unfinished escape")?;
                    self.at += 1;
                    match e {
                        'n' => out.push('\n'),
                        't' => out.push('\t'),
                        'r' => out.push('\r'),
                        'b' => out.push('\u{8}'),
                        'f' => out.push('\u{c}'),
                        'u' => {
                            let mut code = self.hex4()?;
                            if (0xD800..0xDC00).contains(&code) && self.peek() == Some('\\') {
                                self.at += 2; // the "\u" of the second half of a surrogate pair
                                let low = self.hex4()?;
                                code = 0x10000 + ((code - 0xD800) << 10) + (low.wrapping_sub(0xDC00) & 0x3FF);
                            }
                            out.push(char::from_u32(code).unwrap_or('\u{FFFD}'));
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
    }

    fn value(&mut self) -> Result<Json, String> {
        self.space();
        match self.peek() {
            Some('{') => {
                self.at += 1;
                let mut pairs = Vec::new();
                loop {
                    self.space();
                    if self.peek() == Some('}') {
                        self.at += 1;
                        return Ok(Json::Object(pairs));
                    }
                    let key = self.string()?;
                    self.space();
                    if self.peek() != Some(':') {
                        return Err("expected ':'".to_string());
                    }
                    self.at += 1;
                    pairs.push((key, self.value()?));
                    self.space();
                    if self.peek() == Some(',') {
                        self.at += 1;
                    }
                }
            }
            Some('"') => Ok(Json::Text(self.string()?)),
            Some('[') => Err("lists are not supported in app input".to_string()),
            Some(_) => {
                // a bare number, true, false or null: kept as its text
                let start = self.at;
                while self.peek().is_some_and(|c| c != ',' && c != '}' && !c.is_whitespace()) {
                    self.at += 1;
                }
                let raw: String = self.chars[start..self.at].iter().collect();
                Ok(if raw == "null" { Json::Null } else { Json::Text(raw) })
            }
            None => Err("empty input".to_string()),
        }
    }
}

/// Reads `{"payload": {"field": "value", ...}, "reply": null | "text"}`.
pub fn parse_input(text: &str) -> Result<Input, String> {
    let mut parser = Parser { chars: text.chars().collect(), at: 0 };
    let Json::Object(top) = parser.value()? else {
        return Err("expected an object".to_string());
    };
    let mut input = Input { payload: BTreeMap::new(), reply: None };
    for (key, value) in top {
        match (key.as_str(), value) {
            ("payload", Json::Object(fields)) => {
                for (name, v) in fields {
                    if let Json::Text(t) = v {
                        input.payload.insert(name, t);
                    }
                }
            }
            ("reply", Json::Text(t)) => input.reply = Some(t),
            _ => {}
        }
    }
    Ok(input)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_payload_reply_and_escapes() {
        let input = parse_input(r#"{"payload": {"a": " x\ny ", "n": 3, "e": "é😀\"q\""}, "reply": null}"#).unwrap();
        assert_eq!(input.get("a"), "x\ny");
        assert_eq!(input.get("n"), "3");
        assert_eq!(input.get("e"), "é😀\"q\"");
        assert_eq!(input.get("missing"), "");
        assert!(input.reply.is_none());
        assert_eq!(parse_input(r#"{"payload":{},"reply":"hi"}"#).unwrap().reply.as_deref(), Some("hi"));
        assert!(parse_input("[1]").is_err());
    }

    #[test]
    fn steps_print_as_one_line() {
        assert_eq!(step_json(&answer("a\n\"b\"")), r#"{"output":{"answer":"a\n\"b\""}}"#);
        assert_eq!(step_json(&Step::Error("no".into())), r#"{"error":"no"}"#);
        assert_eq!(step_json(&ask("t", "p", &[("k", "v")])), r#"{"llm":{"task":"t","prompt":"p","vars":{"k":"v"}}}"#);
        assert!(!step_json(&answer("line1\nline2")).contains('\n'));
    }
}
