use crate::github::Comment;
use pulldown_cmark::{Event, Parser, Tag, TagEnd};

/// Inspect rendered text, excluding fenced/indented code and inline code.
/// This is a local mention heuristic, not GitHub's notification parser.
pub fn mentions(body: &str, username: &str) -> bool {
    let needle = format!("@{}", username.to_ascii_lowercase());
    let mut in_code = false;
    Parser::new(body).any(|event| match event {
        Event::Start(Tag::CodeBlock(_)) => {
            in_code = true;
            false
        }
        Event::End(TagEnd::CodeBlock) => {
            in_code = false;
            false
        }
        Event::Text(text) if !in_code => {
            let text = text.to_ascii_lowercase();
            text.match_indices(&needle).any(|(offset, _)| {
                let before = text[..offset].chars().next_back();
                let after = text[offset + needle.len()..].chars().next();
                !before.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-' | '@'))
                    && !after.is_some_and(|c| c.is_alphanumeric() || matches!(c, '_' | '-'))
            })
        }
        _ => false,
    })
}

pub fn reason(
    comment: &Comment,
    review_comments: &[Comment],
    username: &str,
) -> Option<&'static str> {
    if comment.user.login.eq_ignore_ascii_case(username) {
        return None;
    }
    if mentions(&comment.body, username) {
        return Some("Mentioned you");
    }
    let thread = comment.thread_id?;
    review_comments
        .iter()
        .any(|previous| {
            previous.thread_id == Some(thread)
                && previous.user.login.eq_ignore_ascii_case(username)
                && previous.created_at < comment.created_at
        })
        .then_some("Replied in your review conversation")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::User;
    use chrono::{TimeZone, Utc};

    fn comment(id: u64, author: &str, thread: u64) -> Comment {
        Comment {
            id: format!("review:{id}"),
            user: User {
                login: author.into(),
            },
            body: "A reply".into(),
            html_url: "https://github.com/a/b/pull/1".into(),
            created_at: Utc.timestamp_opt(id as i64, 0).unwrap(),
            thread_id: Some(thread),
        }
    }

    #[test]
    fn mention_boundaries_and_markdown() {
        assert!(mentions("Thanks, **@Warren**!", "warren"));
        assert!(!mentions("@warren-other test@warren.com", "warren"));
        assert!(!mentions("`@warren`\n\n```\n@warren\n```", "warren"));
        assert!(mentions("[ @warren ](https://github.com/warren)", "warren"));
    }

    #[test]
    fn participation_is_specific_to_thread_and_precedes_reply() {
        let own = comment(2, "warren", 10);
        let comments = vec![own];
        assert!(reason(&comment(3, "someone", 10), &comments, "warren").is_some());
        assert!(reason(&comment(3, "someone", 11), &comments, "warren").is_none());
        assert!(reason(&comment(1, "someone", 10), &comments, "warren").is_none());
        assert!(reason(&comment(4, "warren", 10), &comments, "warren").is_none());
    }
}
