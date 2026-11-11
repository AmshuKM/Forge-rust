//! Document summariser and Q&A. Uses a model: asks for a summary, or an answer when a question is given.
use forge_app::{answer, ask, run, Input, Step};

const MAX_DOC: usize = 20_000; // stays under context_chars so nothing is cut off silently

fn app(input: &Input) -> Step {
    let document = input.get("document");
    let question = input.get("question");
    if document.is_empty() {
        return Step::Error("Please paste a document.".into());
    }
    if document.chars().count() > MAX_DOC {
        return Step::Error("That document is too long (limit 20,000 characters).".into());
    }
    if question.chars().count() > 500 {
        return Step::Error("That question is too long (limit 500 characters).".into());
    }
    let Some(reply) = &input.reply else {
        return if question.is_empty() {
            ask("summarize", "docqa_summarize", &[("document", document)])
        } else {
            ask("answer", "docqa_answer", &[("document", document), ("question", question)])
        };
    };
    let text = reply.trim();
    if text.is_empty() {
        return Step::Invalid("the answer was empty".into());
    }
    if text.chars().count() > 4000 {
        return Step::Invalid("the answer was longer than 4000 characters".into());
    }
    if question.is_empty() && document.chars().count() > 1000 && text.chars().count() >= document.chars().count() {
        return Step::Invalid("the summary was not shorter than the document".into());
    }
    answer(text)
}

fn main() {
    run(app);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn input(document: &str, question: &str, reply: Option<&str>) -> Input {
        let mut i = Input { payload: Default::default(), reply: reply.map(String::from) };
        i.payload.insert("document".into(), document.into());
        i.payload.insert("question".into(), question.into());
        i
    }

    #[test]
    fn rejects_empty_and_oversized_input() {
        assert!(matches!(app(&input("  ", "", None)), Step::Error(_)));
        assert!(matches!(app(&input(&"x".repeat(MAX_DOC + 1), "", None)), Step::Error(_)));
        assert!(matches!(app(&input("ok", &"q".repeat(501), None)), Step::Error(_)));
    }

    #[test]
    fn question_picks_answer_task_and_empty_picks_summary() {
        assert!(matches!(app(&input("d", "q", None)), Step::Ask { task, .. } if task == "answer"));
        assert!(matches!(app(&input("d", "", None)), Step::Ask { task, .. } if task == "summarize"));
    }

    #[test]
    fn checks_the_reply() {
        let doc = "x".repeat(2000);
        assert!(matches!(app(&input(&doc, "", Some(" short "))), Step::Output(_)));
        for bad in ["", &"y".repeat(4001), &"y".repeat(2000)] {
            assert!(matches!(app(&input(&doc, "", Some(bad))), Step::Invalid(_)));
        }
    }
}
