//! Turn a title into a URL slug. Uses no AI model.
use forge_app::{answer, run, Input, Step};

/// Lowercase ASCII letters and digits pass; common accented Latin letters lose their accent.
fn fold(c: char) -> Option<char> {
    Some(match c {
        'a'..='z' | '0'..='9' => c,
        'à'..='å' => 'a',
        'ç' => 'c',
        'è'..='ë' => 'e',
        'ì'..='ï' => 'i',
        'ñ' => 'n',
        'ò'..='ö' | 'ø' => 'o',
        'ù'..='ü' => 'u',
        'ý' | 'ÿ' => 'y',
        _ => return None,
    })
}

fn slug(text: &str) -> String {
    let mut out = String::new();
    for c in text.to_lowercase().chars() {
        if ('\u{300}'..='\u{36f}').contains(&c) {
            continue; // a combining accent: the letter before it is already kept
        }
        match fold(c) {
            Some(c) => out.push(c),
            None if !out.is_empty() && !out.ends_with('-') => out.push('-'),
            None => {}
        }
    }
    out.trim_end_matches('-').to_string()
}

fn app(input: &Input) -> Step {
    let text = input.get("text");
    if text.is_empty() {
        return Step::Error("Please enter a title.".into());
    }
    if text.chars().count() > 200 {
        return Step::Error("That title is too long (limit 200 characters).".into());
    }
    let result = slug(text);
    if result.is_empty() {
        return Step::Error("The title needs at least one letter or digit.".into());
    }
    answer(result)
}

fn main() {
    run(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slug("Hello, World!"), "hello-world");
        assert_eq!(slug("  Cafe\u{301} au lait  "), "cafe-au-lait");
        assert_eq!(slug("Crème Brûlée Recipe"), "creme-brulee-recipe");
        assert_eq!(slug("a--b__c"), "a-b-c");
    }

    #[test]
    fn rejects_unusable_input() {
        for bad in ["", "   ", "!!!"] {
            let mut input = Input { payload: Default::default(), reply: None };
            input.payload.insert("text".into(), bad.into());
            assert!(matches!(app(&input), Step::Error(_)), "{bad:?}");
        }
    }
}
