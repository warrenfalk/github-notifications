use crate::github::{Comment, Conversation, ReviewState};
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

pub fn relevant(conversation: Conversation, username: &str) -> Vec<(Comment, &'static str)> {
    let own_pull_request = conversation
        .pull_request_author
        .as_ref()
        .is_some_and(|author| author.login.eq_ignore_ascii_case(username));
    let mut result = Vec::new();
    for comment in conversation.comments {
        if let Some(reason) = reason(&comment, &conversation.review_comments, username) {
            result.push((comment, reason));
        }
    }
    for (comment, state) in conversation.reviews {
        let reason = if own_pull_request && !comment.user.login.eq_ignore_ascii_case(username) {
            match state {
                ReviewState::Approved => Some("Approved your pull request"),
                ReviewState::ChangesRequested => Some("Requested changes on your pull request"),
                ReviewState::Commented => Some("Left a review on your pull request"),
                _ => None,
            }
        } else {
            None
        }
        .or_else(|| reason(&comment, &conversation.review_comments, username));
        if let Some(reason) = reason {
            result.push((comment, reason));
        }
    }
    for request in conversation.review_requests {
        if request.reviewer.login.eq_ignore_ascii_case(username)
            && !request.comment.user.login.eq_ignore_ascii_case(username)
        {
            result.push((request.comment, "Requested your review"));
        }
    }
    result
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

    #[test]
    fn all_submitted_review_types_qualify_on_own_pr_without_text() {
        for (state, expected) in [
            (ReviewState::Approved, "Approved your pull request"),
            (
                ReviewState::ChangesRequested,
                "Requested changes on your pull request",
            ),
            (ReviewState::Commented, "Left a review on your pull request"),
        ] {
            let mut review = comment(3, "reviewer", 10);
            review.body.clear();
            review.thread_id = None;
            let conversation = Conversation {
                pull_request_author: Some(User {
                    login: "WARREN".into(),
                }),
                reviews: vec![(review, state)],
                ..Conversation::default()
            };
            let accepted = relevant(conversation, "warren");
            assert_eq!(accepted.len(), 1);
            assert_eq!(accepted[0].1, expected);
        }
    }

    #[test]
    fn foreign_prs_self_reviews_and_dismissed_reviews_do_not_qualify() {
        for (author, reviewer, state) in [
            ("other", "reviewer", ReviewState::Approved),
            ("warren", "warren", ReviewState::Commented),
            ("warren", "reviewer", ReviewState::Dismissed),
        ] {
            let conversation = Conversation {
                pull_request_author: Some(User {
                    login: author.into(),
                }),
                reviews: vec![(comment(3, reviewer, 10), state)],
                ..Conversation::default()
            };
            assert!(relevant(conversation, "warren").is_empty());
        }
    }

    #[test]
    fn mentions_in_reviews_on_other_prs_still_qualify() {
        let mut review = comment(3, "reviewer", 10);
        review.body = "Please check this, @warren".into();
        review.thread_id = None;
        let conversation = Conversation {
            reviews: vec![(review, ReviewState::Commented)],
            ..Conversation::default()
        };
        assert_eq!(relevant(conversation, "warren")[0].1, "Mentioned you");
    }

    #[test]
    fn requests_are_filtered_by_actual_reviewer_not_notification_reason() {
        let requests = ["WARREN", "other"]
            .into_iter()
            .map(|reviewer| crate::github::ReviewRequest {
                comment: comment(3, "author", 10),
                reviewer: User {
                    login: reviewer.into(),
                },
            })
            .collect();
        let conversation = Conversation {
            review_requests: requests,
            ..Conversation::default()
        };
        let accepted = relevant(conversation, "warren");
        assert_eq!(accepted.len(), 1);
        assert_eq!(accepted[0].1, "Requested your review");
    }
}
