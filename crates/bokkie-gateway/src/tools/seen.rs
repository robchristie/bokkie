//! A Bot's seen items: the items it has shown its owner, by canonical id,
//! through the `seen` grant's tools, so that it does not show them again.
//!
//! An item is named by an arXiv id, a DOI or a link, and stored by its
//! canonical id ([`canonical`]): `arxiv:<id>` without a version,
//! `doi:<doi>` in lower case, or a normalised `https://` link. arXiv's own
//! links and DOIs, and Hugging Face paper pages, become the arXiv id, so
//! the same paper found in different places is one item. Items are kept
//! [`RETENTION_MS`] from when they were last shown, at most
//! [`MAX_PER_BOT`] per Bot, the oldest going first.
//!
//! Every query names the owning user, and a Bot's items are reached only
//! through a Bot of that user.

use std::collections::HashMap;

use bokkie_protocol::{BotId, Timestamp, UserId};
use serde_json::{Value, json};
use sqlx::Row;
use sqlx::sqlite::{SqliteConnection, SqliteExecutor};

use super::{Caller, Output};
use crate::clock::DAY_MS;
use crate::error::{Failure, Result};

/// How long an item is kept after it was last shown.
pub const RETENTION_MS: i64 = 365 * DAY_MS;
/// The most items one call checks or marks.
pub const MAX_PER_CALL: usize = 50;
/// The most items kept per Bot; marking more drops the oldest.
pub const MAX_PER_BOT: i64 = 10_000;
/// The longest title kept, in characters; longer ones are cut.
pub const TITLE_CHARS: usize = 300;
/// The longest link accepted, in characters.
pub const URL_CHARS: usize = 2_048;
/// How much of an unrecognised item is repeated back, in characters.
const ECHO_CHARS: usize = 200;

/// Query parameters that only track where a link was shared, removed from
/// links along with every `utm_` parameter.
const TRACKING: &[&str] = &[
    "fbclid", "gclid", "dclid", "gbraid", "wbraid", "msclkid", "yclid", "mc_cid", "mc_eid",
    "igshid", "_hsenc", "_hsmi", "mkt_tok",
];

/// The canonical id of an item named by an arXiv id, a DOI or an http(s)
/// link, or `None` if it is none of them:
///
/// - **arXiv ids**, such as `arXiv:2401.12345v2`, `2401.12345` or
///   `hep-th/9901001`, and arXiv's own links (`/abs/`, `/pdf/` and `/html/`
///   on `arxiv.org`), become `arxiv:<id>` without the version, so every
///   version of a paper is one item. So do arXiv's DOIs
///   (`10.48550/arXiv.<id>`) and Hugging Face paper pages
///   (`huggingface.co/papers/<id>`).
/// - **DOIs**, such as `10.1000/xyz`, `doi:10.1000/xyz` or a `doi.org`
///   link, become `doi:<doi>` in lower case: DOIs ignore case.
/// - **Links** use http or https, and become `https://<host><path>?<query>`:
///   the host in lower case without a leading `www.` or a default port,
///   http and https alike, no user name, password or fragment, tracking
///   parameters removed, the other parameters sorted, and no trailing `/`
///   after a path. A link without a scheme is read as https.
pub fn canonical(input: &str) -> Option<String> {
    let input = input.trim();
    if input.is_empty() || input.len() > URL_CHARS {
        return None;
    }
    if let Some(rest) = strip_prefix_ignoring_case(input, "arxiv:") {
        return arxiv_id(rest.trim()).map(|id| format!("arxiv:{id}"));
    }
    if let Some(rest) = strip_prefix_ignoring_case(input, "doi:") {
        return doi(rest.trim());
    }
    if let Some(id) = arxiv_id(input) {
        return Some(format!("arxiv:{id}"));
    }
    if input.starts_with("10.") {
        return doi(input);
    }
    link(input)
}

fn strip_prefix_ignoring_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix)
        .then(|| &text[prefix.len()..])
}

fn all_digits(text: &str, lengths: std::ops::RangeInclusive<usize>) -> bool {
    lengths.contains(&text.len()) && text.bytes().all(|b| b.is_ascii_digit())
}

/// An arXiv id without its version, if `text` is one: `YYMM.NNNNN` (four or
/// five digits after the dot) or the older `archive[.XX]/YYMMNNN`, each
/// with an optional `vN`.
fn arxiv_id(text: &str) -> Option<String> {
    let unversioned = match text.rfind('v') {
        Some(at) if at > 0 && all_digits(&text[at + 1..], 1..=4) => &text[..at],
        _ => text,
    };
    if let Some((year_month, number)) = unversioned.split_once('.')
        && all_digits(year_month, 4..=4)
        && all_digits(number, 4..=5)
    {
        return Some(unversioned.to_owned());
    }
    let (archive, number) = unversioned.split_once('/')?;
    if !all_digits(number, 7..=7) {
        return None;
    }
    let (name, subject) = match archive.split_once('.') {
        Some((name, subject)) => (name, Some(subject)),
        None => (archive, None),
    };
    let name_ok = !name.is_empty()
        && !name.starts_with('-')
        && !name.ends_with('-')
        && name.bytes().all(|b| b.is_ascii_alphabetic() || b == b'-');
    let subject_ok =
        subject.is_none_or(|s| s.len() == 2 && s.bytes().all(|b| b.is_ascii_alphabetic()));
    if !(name_ok && subject_ok) {
        return None;
    }
    Some(match subject {
        Some(subject) => format!(
            "{}.{}/{number}",
            name.to_ascii_lowercase(),
            subject.to_ascii_uppercase()
        ),
        None => format!("{}/{number}", name.to_ascii_lowercase()),
    })
}

/// The canonical id of a DOI: `10.<registrant>/<suffix>`, in lower case.
/// arXiv's DOIs become the arXiv id.
fn doi(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let (prefix, suffix) = lower.split_once('/')?;
    let registrant = prefix.strip_prefix("10.")?;
    let registrant_ok = !registrant.is_empty()
        && registrant.bytes().all(|b| b.is_ascii_digit() || b == b'.')
        && !registrant.ends_with('.');
    if !registrant_ok || suffix.is_empty() || suffix.chars().any(char::is_whitespace) {
        return None;
    }
    if prefix == "10.48550"
        && let Some(id) = suffix.strip_prefix("arxiv.").and_then(arxiv_id)
    {
        return Some(format!("arxiv:{id}"));
    }
    Some(format!("doi:{lower}"))
}

/// The arXiv id in the path of a link on `arxiv.org`.
fn arxiv_path(path: &str) -> Option<String> {
    let rest = ["/abs/", "/pdf/", "/html/"]
        .iter()
        .find_map(|prefix| path.strip_prefix(prefix))?;
    let rest = rest.trim_end_matches('/');
    arxiv_id(rest.strip_suffix(".pdf").unwrap_or(rest))
}

/// The canonical id of an http(s) link.
fn link(input: &str) -> Option<String> {
    let parsed = match url::Url::parse(input) {
        Ok(url) => url,
        // No scheme: read it as https, as a browser would, if it names a
        // host with a dot. A lone word is not a link.
        Err(url::ParseError::RelativeUrlWithoutBase) => {
            let url = url::Url::parse(&format!("https://{input}")).ok()?;
            if !url.host_str().is_some_and(|host| host.contains('.')) {
                return None;
            }
            url
        }
        Err(_) => return None,
    };
    if !matches!(parsed.scheme(), "http" | "https") {
        return None;
    }
    let host = parsed.host_str()?.trim_end_matches('.');
    let host = host.strip_prefix("www.").unwrap_or(host);
    if host.is_empty() {
        return None;
    }
    let path = parsed.path();
    match host {
        "arxiv.org" | "export.arxiv.org" => {
            if let Some(id) = arxiv_path(path) {
                return Some(format!("arxiv:{id}"));
            }
        }
        "doi.org" | "dx.doi.org" => {
            let decoded = percent_encoding::percent_decode_str(path.trim_start_matches('/'))
                .decode_utf8()
                .ok()?;
            if let Some(doi) = doi(&decoded) {
                return Some(doi);
            }
        }
        "huggingface.co" => {
            if let Some(rest) = path.strip_prefix("/papers/")
                && let Some(id) = arxiv_id(rest.trim_end_matches('/'))
            {
                return Some(format!("arxiv:{id}"));
            }
        }
        _ => {}
    }
    // Links on any port but the default for their scheme keep it.
    let port = parsed.port().map(|p| format!(":{p}")).unwrap_or_default();
    let path = match path.trim_end_matches('/') {
        "" => "/",
        trimmed => trimmed,
    };
    let mut pairs: Vec<(String, String)> = parsed
        .query_pairs()
        .filter(|(key, _)| {
            let key = key.to_ascii_lowercase();
            !key.starts_with("utm_") && !TRACKING.contains(&key.as_str())
        })
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    pairs.sort();
    let query = if pairs.is_empty() {
        String::new()
    } else {
        let mut query = url::form_urlencoded::Serializer::for_suffix(String::from("?"), 1);
        query.extend_pairs(&pairs);
        query.finish()
    };
    Some(format!("https://{host}{port}{path}{query}"))
}

/// A link to an item by its canonical id.
fn link_to(id: &str) -> String {
    if let Some(arxiv) = id.strip_prefix("arxiv:") {
        format!("https://arxiv.org/abs/{arxiv}")
    } else if let Some(doi) = id.strip_prefix("doi:") {
        format!("https://doi.org/{doi}")
    } else {
        id.to_owned()
    }
}

/// An item as it was last shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shown {
    pub title: String,
    pub url: Option<String>,
    pub shown_at: Timestamp,
}

/// An item to mark as shown.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The canonical id.
    pub id: String,
    pub title: String,
    pub url: String,
}

/// Which of these canonical ids the Bot showed within [`RETENTION_MS`] of
/// `now`, with how each was last shown.
pub async fn shown(
    db: impl SqliteExecutor<'_> + Copy,
    now: Timestamp,
    user: UserId,
    bot: BotId,
    ids: &[String],
) -> Result<HashMap<String, Shown>> {
    let mut shown = HashMap::new();
    for id in ids {
        let row = sqlx::query(
            "SELECT title, url, shown_at FROM seen_items
             WHERE bot_id = ? AND user_id = ? AND item = ? AND shown_at > ?",
        )
        .bind(bot.0)
        .bind(user.0)
        .bind(id)
        .bind(now.0 - RETENTION_MS)
        .fetch_optional(db)
        .await?;
        if let Some(row) = row {
            shown.insert(
                id.clone(),
                Shown {
                    title: row.get("title"),
                    url: row.get("url"),
                    shown_at: Timestamp(row.get("shown_at")),
                },
            );
        }
    }
    Ok(shown)
}

/// Marks items as shown by the Bot at `now`, keeping at most
/// [`MAX_PER_BOT`]. Returns whether the Bot is the user's. `conn` must be
/// in a write transaction.
pub async fn mark(
    conn: &mut SqliteConnection,
    now: Timestamp,
    user: UserId,
    bot: BotId,
    items: &[Item],
) -> Result<bool> {
    let mut theirs = true;
    for item in items {
        let done = sqlx::query(
            "INSERT INTO seen_items (bot_id, user_id, item, title, url, shown_at)
             SELECT id, user_id, ?, ?, ?, ? FROM bots WHERE id = ? AND user_id = ?
             ON CONFLICT (bot_id, item) DO UPDATE SET title = excluded.title,
                 url = excluded.url, shown_at = excluded.shown_at
               WHERE seen_items.user_id = excluded.user_id",
        )
        .bind(&item.id)
        .bind(&item.title)
        .bind(&item.url)
        .bind(now.0)
        .bind(bot.0)
        .bind(user.0)
        .execute(&mut *conn)
        .await?;
        theirs &= done.rows_affected() > 0;
    }
    sqlx::query(
        "DELETE FROM seen_items WHERE bot_id = ?1 AND user_id = ?2 AND rowid IN (
             SELECT rowid FROM seen_items WHERE bot_id = ?1 AND user_id = ?2
             ORDER BY shown_at DESC, rowid DESC LIMIT -1 OFFSET ?3)",
    )
    .bind(bot.0)
    .bind(user.0)
    .bind(MAX_PER_BOT)
    .execute(&mut *conn)
    .await?;
    Ok(theirs)
}

/// How many items the Bot keeps.
pub async fn count(db: impl SqliteExecutor<'_>, user: UserId, bot: BotId) -> Result<i64> {
    Ok(
        sqlx::query_scalar("SELECT count(*) FROM seen_items WHERE bot_id = ? AND user_id = ?")
            .bind(bot.0)
            .bind(user.0)
            .fetch_one(db)
            .await?,
    )
}

/// Deletes items last shown more than [`RETENTION_MS`] ago. Returns how
/// many.
pub async fn prune(conn: &mut SqliteConnection, now: Timestamp) -> Result<u64> {
    let done = sqlx::query("DELETE FROM seen_items WHERE shown_at <= ?")
        .bind(now.0 - RETENTION_MS)
        .execute(conn)
        .await?;
    Ok(done.rows_affected())
}

fn internal(failure: Failure) -> String {
    tracing::error!(message = %failure.message, "a seen tool failed");
    "Your seen items could not be reached just now.".into()
}

fn echo(text: &str) -> String {
    let mut echoed: String = text.chars().take(ECHO_CHARS).collect();
    if echoed.len() < text.len() {
        echoed.push('…');
    }
    echoed
}

const NOT_AN_ITEM: &str = "not an arXiv id, DOI or http(s) link";

/// The `items` array of a call, within [`MAX_PER_CALL`].
fn items_of<'a>(tool: &str, arguments: &'a Value) -> std::result::Result<&'a [Value], String> {
    let items = arguments["items"]
        .as_array()
        .ok_or_else(|| format!("{tool} needs {{\"items\": [...]}}."))?;
    if items.is_empty() {
        return Err(format!("{tool} needs at least one item."));
    }
    if items.len() > MAX_PER_CALL {
        return Err(format!(
            "{tool} takes at most {MAX_PER_CALL} items at a time; split them up."
        ));
    }
    Ok(items)
}

/// `seen_check`: which items are new, and how the others were last shown.
pub async fn check_tool(caller: &Caller, arguments: &Value) -> Output {
    let items = items_of("seen_check", arguments)?;
    let mut inputs = Vec::with_capacity(items.len());
    for item in items {
        let text = item
            .as_str()
            .ok_or("seen_check takes each item as a string: an arXiv id, a DOI or a link.")?;
        inputs.push((text, canonical(text)));
    }
    let ids: Vec<String> = inputs.iter().filter_map(|(_, id)| id.clone()).collect();
    let now = caller.clock.now();
    let shown = shown(&caller.db, now, caller.user, caller.bot, &ids)
        .await
        .map_err(internal)?;
    let results: Vec<Value> = inputs
        .iter()
        .map(|(text, id)| match id {
            None => json!({"item": echo(text), "error": NOT_AN_ITEM}),
            Some(id) => match shown.get(id) {
                None => json!({"item": echo(text), "id": id, "seen": false}),
                Some(seen) => json!({
                    "item": echo(text), "id": id, "seen": true, "title": seen.title,
                    "url": seen.url, "shown_days_ago": (now.0 - seen.shown_at.0) / DAY_MS,
                }),
            },
        })
        .collect();
    Ok(json!({"items": results}).to_string())
}

/// `seen_mark`: records items as shown now. Every item must be valid, or
/// none is marked.
pub async fn mark_tool(caller: &Caller, arguments: &Value) -> Output {
    let items = items_of("seen_mark", arguments)?;
    let mut marked = Vec::with_capacity(items.len());
    for item in items {
        let named = item["id"]
            .as_str()
            .ok_or("seen_mark takes each item as {\"id\": ..., \"title\": ...}.")?;
        let id = canonical(named)
            .ok_or_else(|| format!("Nothing was marked: \"{}\" is {NOT_AN_ITEM}.", echo(named)))?;
        let title = item["title"].as_str().map(str::trim).unwrap_or_default();
        if title.is_empty() {
            return Err(format!(
                "Nothing was marked: \"{}\" needs a title.",
                echo(named)
            ));
        }
        let url = match item.get("url").and_then(Value::as_str) {
            Some(url) => {
                if url.len() > URL_CHARS || link(url).is_none() {
                    return Err(format!(
                        "Nothing was marked: the url of \"{}\" is not an http(s) link.",
                        echo(named)
                    ));
                }
                url.trim().to_owned()
            }
            None => link_to(&id),
        };
        marked.push(Item {
            id,
            title: title.chars().take(TITLE_CHARS).collect(),
            url,
        });
    }
    let mut tx = crate::db::write(&caller.db)
        .await
        .map_err(|e| internal(e.into()))?;
    mark(
        &mut tx,
        caller.clock.now(),
        caller.user,
        caller.bot,
        &marked,
    )
    .await
    .map_err(internal)?;
    tx.commit().await.map_err(|e| internal(e.into()))?;
    let ids: Vec<&str> = marked.iter().map(|item| item.id.as_str()).collect();
    Ok(format!(
        "Marked {} as shown: {}.",
        if ids.len() == 1 {
            "1 item".to_owned()
        } else {
            format!("{} items", ids.len())
        },
        ids.join(", ")
    ))
}

/// A bounded summary of a seen call for the tool-call log: how many items,
/// and for `seen_mark` their canonical ids.
pub fn summary(tool: &str, arguments: &Value) -> String {
    let Some(items) = arguments["items"].as_array() else {
        return "invalid arguments".into();
    };
    let count = if items.len() == 1 {
        "1 item".to_owned()
    } else {
        format!("{} items", items.len())
    };
    if tool != "seen_mark" {
        return count;
    }
    let ids: Vec<String> = items
        .iter()
        .take(MAX_PER_CALL)
        .filter_map(|item| item["id"].as_str().and_then(canonical))
        .collect();
    if ids.is_empty() {
        count
    } else {
        format!("{count}: {}", ids.join(", "))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn id(input: &str) -> Option<String> {
        canonical(input)
    }

    #[test]
    fn arxiv_ids_in_any_form_become_one_id_without_the_version() {
        for form in [
            "2401.12345",
            "2401.12345v3",
            "arXiv:2401.12345",
            "ARXIV:2401.12345v1",
            " arxiv: 2401.12345 ",
            "https://arxiv.org/abs/2401.12345",
            "http://arxiv.org/abs/2401.12345v2",
            "https://www.arxiv.org/abs/2401.12345/",
            "https://arxiv.org/pdf/2401.12345v2",
            "https://arxiv.org/pdf/2401.12345v2.pdf",
            "https://arxiv.org/html/2401.12345v1",
            "https://export.arxiv.org/abs/2401.12345#section",
            "arxiv.org/abs/2401.12345?context=cs.LG",
            "https://huggingface.co/papers/2401.12345",
            "10.48550/arXiv.2401.12345",
            "doi:10.48550/ARXIV.2401.12345",
            "https://doi.org/10.48550/arXiv.2401.12345",
        ] {
            assert_eq!(id(form).as_deref(), Some("arxiv:2401.12345"), "{form}");
        }
        assert_eq!(id("1501.0001").as_deref(), Some("arxiv:1501.0001"));
        for old in [
            "hep-th/9901001",
            "arXiv:hep-th/9901001v2",
            "https://arxiv.org/abs/hep-th/9901001",
        ] {
            assert_eq!(id(old).as_deref(), Some("arxiv:hep-th/9901001"), "{old}");
        }
        assert_eq!(
            id("math.gt/0309136").as_deref(),
            Some("arxiv:math.GT/0309136")
        );
        // Not arXiv ids.
        for not in [
            "arXiv:2401.123",
            "arXiv:24012.12345",
            "arXiv:2401.123456",
            "arXiv:hep-th/990100",
            "arXiv:-th/9901001",
        ] {
            assert_eq!(id(not), None, "{not}");
        }
        // Another arXiv page is only a link.
        assert_eq!(
            id("https://arxiv.org/list/cs.LG/new").as_deref(),
            Some("https://arxiv.org/list/cs.LG/new")
        );
    }

    #[test]
    fn dois_ignore_case_and_their_links_become_the_doi() {
        for form in [
            "10.1145/3600006.3613165",
            "doi:10.1145/3600006.3613165",
            "DOI: 10.1145/3600006.3613165",
            "https://doi.org/10.1145/3600006.3613165",
            "http://dx.doi.org/10.1145/3600006.3613165",
        ] {
            assert_eq!(
                id(form).as_deref(),
                Some("doi:10.1145/3600006.3613165"),
                "{form}"
            );
        }
        assert_eq!(
            id("10.1038/S41586-023-06291-2").as_deref(),
            Some("doi:10.1038/s41586-023-06291-2")
        );
        // A DOI holds no whitespace, so this stays a link.
        assert_eq!(
            id("https://doi.org/10.1000/a%2Fb%20c").as_deref(),
            Some("https://doi.org/10.1000/a%2Fb%20c")
        );
        assert_eq!(
            id("https://doi.org/10.1000/a%2Fb").as_deref(),
            Some("doi:10.1000/a/b")
        );
        for not in ["10.abc/x", "10./x", "10.1000/", "doi:11.1000/x"] {
            assert_eq!(id(not), None, "{not}");
        }
    }

    #[test]
    fn links_are_normalised() {
        let same = [
            "https://example.com/post",
            "http://example.com/post",
            "HTTPS://WWW.Example.COM/post/",
            "https://example.com:443/post",
            "http://example.com:80/post",
            "https://user:secret@example.com/post",
            "https://example.com/post#comments",
            "https://example.com/a/../post",
            "https://example.com/post?utm_source=feed&utm_medium=rss&fbclid=x",
            "example.com/post",
            "www.example.com/post",
        ];
        for form in same {
            assert_eq!(
                id(form).as_deref(),
                Some("https://example.com/post"),
                "{form}"
            );
        }
        assert_eq!(
            id("https://example.com").as_deref(),
            Some("https://example.com/")
        );
        // With a scheme, any host will do.
        assert_eq!(
            id("http://intranet/x").as_deref(),
            Some("https://intranet/x")
        );
        assert_eq!(
            id("https://example.com/").as_deref(),
            Some("https://example.com/")
        );
        // Other parameters stay, sorted; other ports stay; case in the path
        // matters.
        assert_eq!(
            id("https://news.example.com/item?id=42&b=two%20words&a=1&utm_campaign=z").as_deref(),
            Some("https://news.example.com/item?a=1&b=two+words&id=42")
        );
        assert_eq!(
            id("http://example.com:8080/Post").as_deref(),
            Some("https://example.com:8080/Post")
        );
        // International hosts in their ASCII form.
        assert_eq!(
            id("https://bücher.example/x").as_deref(),
            Some("https://xn--bcher-kva.example/x")
        );
        for not in [
            "",
            "   ",
            "ftp://example.com/file",
            "mailto:someone@example.com",
            "javascript:alert(1)",
            "file:///etc/passwd",
            "not a link",
            "nonsense",
            "nonsense/path",
        ] {
            assert_eq!(id(not), None, "{not:?}");
        }
        assert_eq!(
            id(&format!("https://example.com/{}", "x".repeat(URL_CHARS))),
            None
        );
    }

    #[test]
    fn items_link_to_their_canonical_page() {
        assert_eq!(
            link_to("arxiv:2401.12345"),
            "https://arxiv.org/abs/2401.12345"
        );
        assert_eq!(link_to("doi:10.1000/x"), "https://doi.org/10.1000/x");
        assert_eq!(link_to("https://example.com/"), "https://example.com/");
    }

    #[test]
    fn summaries_count_items_and_name_marked_ids() {
        assert_eq!(
            summary("seen_check", &json!({"items": ["2401.12345", "x"]})),
            "2 items"
        );
        assert_eq!(
            summary(
                "seen_mark",
                &json!({"items": [{"id": "arXiv:2401.12345v2", "title": "T"}]})
            ),
            "1 item: arxiv:2401.12345"
        );
        assert_eq!(summary("seen_mark", &json!({})), "invalid arguments");
    }
}
