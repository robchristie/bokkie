//! Codex's web search, which `web.search` brings.
//!
//! It is not a Bokkie tool. Depending on the model, the provider runs it
//! within its response (hosted search), or the app server runs Codex's
//! `web.run` tool by asking the provider's search endpoint; either way the
//! app server reports each search as a `webSearch` item.
//!
//! A thread of a Bot granted `web.search` turns it on through its
//! configuration overrides ([`crate::codex::config::thread_overrides`]);
//! on any other thread a `webSearch` item is a violation
//! ([`crate::codex::guard::check_item`]). The gateway records each
//! completed search in the Bot's tool-call log, by its query or the page
//! it opened, and keeps searches out of the conversation: the reply says
//! what the Bot found.

use serde_json::Value;

/// The name searches go by in the tool-call log.
pub const TOOL: &str = "web_search";

/// The tool-call log's summary of a search: what it looked for or opened,
/// on one line, bounded. The query is what the Bot chose to send outside
/// the household, so its owner sees it; it never reaches the gateway's
/// own log.
pub fn summary(query: &str, action: Option<&Value>) -> String {
    let text = |key: &str| {
        action
            .and_then(|a| a.get(key))
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
    };
    let kind = action.and_then(|a| a.get("type")).and_then(Value::as_str);
    let line = match kind {
        Some("openPage") => format!("open {}", text("url").unwrap_or("a page")),
        Some("findInPage") => format!(
            "find {:?} in {}",
            text("pattern").unwrap_or(""),
            text("url").unwrap_or("a page")
        ),
        _ => {
            let queries: Vec<&str> = action
                .and_then(|a| a.get("queries"))
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .filter(|q| !q.trim().is_empty())
                .collect();
            let query = text("query").or(Some(query).filter(|q| !q.trim().is_empty()));
            match (query, queries.as_slice()) {
                (_, [_, _, ..]) => format!("search {}", queries.join(" | ")),
                (Some(query), _) => format!("search {query}"),
                (None, [only]) => format!("search {only}"),
                (None, _) => "search".into(),
            }
        }
    };
    let single: String = line
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    let mut bounded: String = single.chars().take(SUMMARY_CHARS).collect();
    if bounded.len() < single.len() {
        bounded.pop();
        bounded.push('…');
    }
    bounded
}

/// The longest summary, in characters: the tool-call log's own bound.
const SUMMARY_CHARS: usize = super::audit::SUMMARY_CHARS;

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_search_is_summarised_by_what_it_looked_for() {
        let search = json!({"type": "search", "query": "new arXiv papers on RL"});
        assert_eq!(
            summary("new arXiv papers on RL", Some(&search)),
            "search new arXiv papers on RL"
        );
        // The item's query stands in when the action has none.
        assert_eq!(
            summary("fallback", Some(&json!({"type": "search"}))),
            "search fallback"
        );
        assert_eq!(summary("plain", None), "search plain");
        assert_eq!(summary("", None), "search");
        assert_eq!(
            summary("", Some(&json!({"type": "search", "queries": ["a", "b"]}))),
            "search a | b"
        );
        assert_eq!(
            summary("", Some(&json!({"type": "search", "queries": ["only"]}))),
            "search only"
        );
        assert_eq!(
            summary(
                "",
                Some(&json!({"type": "openPage", "url": "https://example.org/a"}))
            ),
            "open https://example.org/a"
        );
        assert_eq!(
            summary(
                "",
                Some(
                    &json!({"type": "findInPage", "url": "https://example.org/a",
                             "pattern": "Results"})
                )
            ),
            "find \"Results\" in https://example.org/a"
        );
        assert_eq!(summary("q", Some(&json!({"type": "other"}))), "search q");
    }

    #[test]
    fn summaries_are_one_bounded_line() {
        let long = "word ".repeat(1_000);
        let text = summary(&long, None);
        assert_eq!(text.chars().count(), SUMMARY_CHARS);
        assert!(text.ends_with('…'));
        assert_eq!(summary("two\nlines\u{7}", None), "search two lines ");
    }
}
