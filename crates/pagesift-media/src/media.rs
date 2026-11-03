//! Explicit media protocol adapters. An adapter is selected by the caller;
//! unknown sites never silently fall back to scraping arbitrary JSON URLs.
use std::collections::BTreeSet;

use anyhow::{Context, Result, bail, ensure};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use url::Url;

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaAdapter {
    WordPress,
    Mastodon,
    Wikimedia,
    JsonLd,
}

impl std::str::FromStr for MediaAdapter {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, String> {
        match s {
            "wordpress" => Ok(Self::WordPress),
            "mastodon" => Ok(Self::Mastodon),
            "wikimedia" => Ok(Self::Wikimedia),
            "json-ld" => Ok(Self::JsonLd),
            _ => Err("adapter must be wordpress, mastodon, wikimedia or json-ld".into()),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct MediaItem {
    pub source: String,
    pub id: String,
    pub revision: String,
    pub url: String,
    pub title: String,
    pub extension: String,
    pub metadata: Value,
}

pub fn extract_media(
    adapter: MediaAdapter,
    response_url: &Url,
    body: &str,
) -> Result<Vec<MediaItem>> {
    ensure!(body.len() <= 4 * 1024 * 1024, "media listing exceeds 4 MiB");
    ensure!(
        matches!(response_url.scheme(), "http" | "https")
            && response_url.username().is_empty()
            && response_url.password().is_none(),
        "invalid media source URL"
    );
    let mut out = Vec::new();
    let origin = response_url.origin().ascii_serialization();
    match adapter {
        MediaAdapter::WordPress => {
            ensure!(
                response_url.path().contains("/wp-json/wp/v2/media"),
                "WordPress adapter requires its media REST endpoint"
            );
            let value: Value = serde_json::from_str(body)?;
            let rows: Vec<&Value> = if let Some(a) = value.as_array() {
                a.iter().collect()
            } else {
                vec![&value]
            };
            ensure!(rows.len() <= 1024, "too many media items");
            for row in rows {
                let id = identity(&row["id"])?;
                let url = row["source_url"]
                    .as_str()
                    .context("WordPress media lacks source_url")?;
                let title = row["slug"].as_str().unwrap_or(&id).to_owned();
                let metadata = json!({"adapter":"wordpress", "mime_type":row["mime_type"], "modified_gmt":row["modified_gmt"], "caption":row.pointer("/caption/rendered"), "alt_text":row["alt_text"]});
                out.push(item(
                    format!("wordpress:{origin}"),
                    id,
                    url,
                    &title,
                    metadata,
                    response_url,
                )?);
            }
        }
        MediaAdapter::Mastodon => {
            ensure!(
                response_url.path().starts_with("/api/v1/statuses/"),
                "Mastodon adapter requires a status REST endpoint"
            );
            let row: Value = serde_json::from_str(body)?;
            let status = identity(&row["id"])?;
            let media = row["media_attachments"]
                .as_array()
                .context("Mastodon media_attachments missing")?;
            ensure!(media.len() <= 1024, "too many media items");
            for m in media {
                ensure!(
                    matches!(
                        m["type"].as_str(),
                        Some("image" | "video" | "gifv" | "audio")
                    ),
                    "unsupported attachment type"
                );
                let id = identity(&m["id"])?;
                let url = m["url"]
                    .as_str()
                    .context("attachment original URL unavailable")?;
                let description = m["description"].as_str().unwrap_or(&id).to_owned();
                out.push(item(format!("mastodon:{origin}"), id, url, &description,
                    json!({"adapter":"mastodon","status_id":status,"type":m["type"],"description":m["description"],"meta":m["meta"]}), response_url)?);
            }
        }
        MediaAdapter::Wikimedia => {
            ensure!(
                response_url
                    .host_str()
                    .is_some_and(|h| h == "commons.wikimedia.org" || h.ends_with(".wikipedia.org"))
                    && response_url.path() == "/w/api.php",
                "Wikimedia adapter requires a recognized Action API endpoint"
            );
            let body: Value = serde_json::from_str(body)?;
            let pages = body
                .pointer("/query/pages")
                .context("Wikimedia query.pages missing")?;
            let rows: Vec<_> = if let Some(a) = pages.as_array() {
                a.iter().collect()
            } else {
                pages
                    .as_object()
                    .context("pages must be array or object")?
                    .values()
                    .collect()
            };
            ensure!(rows.len() <= 1024, "too many media items");
            for p in rows {
                ensure!(p.get("missing").is_none(), "Wikimedia file missing");
                let id = identity(&p["pageid"])?;
                let info = p["imageinfo"]
                    .as_array()
                    .and_then(|a| a.first())
                    .context("Wikimedia imageinfo missing")?;
                let title = p["title"].as_str().unwrap_or(&id).to_owned();
                out.push(item(format!("wikimedia:{origin}"), id, info["url"].as_str().context("original file URL missing")?, &title,
                    json!({"adapter":"wikimedia","title":p["title"],"timestamp":info["timestamp"],"sha1":info["sha1"],"mime":info["mime"],"extmetadata":info["extmetadata"]}), response_url)?);
            }
        }
        MediaAdapter::JsonLd => {
            let doc = pagesift_html::Html::parse(body);
            let mut nodes = 0;
            for n in doc.root().descendants() {
                if n.as_element().is_some_and(|e| {
                    e.name() == "script"
                        && e.attr("type")
                            .is_some_and(|s| s.eq_ignore_ascii_case("application/ld+json"))
                }) {
                    let value: Value =
                        serde_json::from_str(&n.text()).context("invalid media JSON-LD")?;
                    jsonld(&value, response_url, 0, &mut nodes, &mut out)?;
                }
            }
        }
    }
    let mut ids = BTreeSet::new();
    ensure!(
        out.iter().all(|i| ids.insert((&i.source, &i.id))),
        "duplicate media identity"
    );
    Ok(out)
}
fn jsonld(
    value: &Value,
    base: &Url,
    depth: usize,
    nodes: &mut usize,
    out: &mut Vec<MediaItem>,
) -> Result<()> {
    *nodes += 1;
    ensure!(
        depth <= 32 && *nodes <= 8192 && out.len() <= 1024,
        "JSON-LD media traversal budget exceeded"
    );
    match value {
        Value::Array(a) => {
            for v in a {
                jsonld(v, base, depth + 1, nodes, out)?;
            }
        }
        Value::Object(o) => {
            let media = o.get("@type").is_some_and(|t| {
                t.as_str().is_some_and(media_type)
                    || t.as_array()
                        .is_some_and(|a| a.iter().any(|t| t.as_str().is_some_and(media_type)))
            });
            if media {
                let id = o
                    .get("@id")
                    .and_then(Value::as_str)
                    .context("JSON-LD media requires explicit @id")?;
                ensure!(
                    !id.trim().is_empty() && id.len() <= 2048,
                    "invalid JSON-LD media identity"
                );
                let canonical = base.join(id)?;
                let url = o
                    .get("contentUrl")
                    .and_then(Value::as_str)
                    .context("JSON-LD media contentUrl missing")?;
                out.push(item(format!("jsonld:{}",base.origin().ascii_serialization()), canonical.to_string(), url,
                    o.get("name").and_then(Value::as_str).unwrap_or("media"),
                    json!({"adapter":"json-ld","type":o.get("@type"),"name":o.get("name"),"dateModified":o.get("dateModified"),"license":o.get("license")}),base)?);
            }
            for v in o.values().filter(|v| v.is_object() || v.is_array()) {
                jsonld(v, base, depth + 1, nodes, out)?;
            }
        }
        _ => {}
    }
    Ok(())
}
fn media_type(t: &str) -> bool {
    matches!(
        t.trim_start_matches("https://schema.org/")
            .trim_start_matches("http://schema.org/"),
        "ImageObject" | "VideoObject" | "AudioObject"
    )
}
fn identity(v: &Value) -> Result<String> {
    let s = if let Some(s) = v.as_str() {
        s.to_owned()
    } else if let Some(n) = v.as_u64() {
        n.to_string()
    } else {
        bail!("media identity must be a string or positive integer")
    };
    ensure!(
        !s.trim().is_empty() && s.len() <= 2048,
        "invalid media identity"
    );
    Ok(s)
}
fn item(
    source: String,
    id: String,
    target: &str,
    title: &str,
    metadata: Value,
    base: &Url,
) -> Result<MediaItem> {
    let url = base.join(target)?;
    ensure!(
        matches!(url.scheme(), "http" | "https")
            && url.username().is_empty()
            && url.password().is_none(),
        "media URL must be credential-free HTTP(S)"
    );
    let extension = url
        .path()
        .rsplit('.')
        .next()
        .filter(|s| s.len() <= 8 && !s.is_empty() && s.bytes().all(|c| c.is_ascii_alphanumeric()))
        .unwrap_or("bin")
        .to_ascii_lowercase();
    let revision = blake3::hash(&serde_json::to_vec(&(&url.as_str(), &metadata))?)
        .to_hex()
        .to_string();
    Ok(MediaItem {
        source,
        id,
        revision,
        url: url.to_string(),
        title: title.chars().take(512).collect(),
        extension,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adapters_preserve_business_identity_across_original_url_changes() {
        let u = Url::parse("https://blog.test/wp-json/wp/v2/media").unwrap();
        let a = extract_media(
            MediaAdapter::WordPress,
            &u,
            r#"[{"id":42,"source_url":"/a.jpg","slug":"café","mime_type":"image/jpeg"}]"#,
        )
        .unwrap();
        let b = extract_media(
            MediaAdapter::WordPress,
            &u,
            r#"[{"id":42,"source_url":"/b.jpg","slug":"café","mime_type":"image/jpeg"}]"#,
        )
        .unwrap();
        assert_eq!(a[0].id, b[0].id);
        assert_ne!(a[0].revision, b[0].revision);
        assert_eq!(a[0].extension, "jpg");
        assert!(
            extract_media(
                MediaAdapter::WordPress,
                &u,
                r#"[{"id":42,"source_url":"/a"},{"id":42,"source_url":"/b"}]"#
            )
            .is_err()
        );
    }
    #[test]
    fn mastodon_wikimedia_and_jsonld_use_originals_and_explicit_ids() {
        let mastodon=extract_media(MediaAdapter::Mastodon,&Url::parse("https://social.test/api/v1/statuses/9").unwrap(),r#"{"id":"9","media_attachments":[{"id":"3","type":"image","url":"https://cdn.test/original.png","preview_url":"https://cdn.test/thumb.png"}]}"#).unwrap();
        assert!(mastodon[0].url.ends_with("original.png"));
        let wiki=extract_media(MediaAdapter::Wikimedia,&Url::parse("https://commons.wikimedia.org/w/api.php").unwrap(),r#"{"query":{"pages":{"12":{"pageid":12,"title":"File:Example.jpg","imageinfo":[{"url":"https://upload.wikimedia.org/example.jpg"}]}}}}"#).unwrap();
        assert_eq!(wiki[0].id, "12");
        let html = r##"<script type="application/ld+json">{"@graph":[{"@type":"ImageObject","@id":"#photo","contentUrl":"/full.jpg"}]}</script>"##;
        let jsonld = extract_media(
            MediaAdapter::JsonLd,
            &Url::parse("https://example.test/story").unwrap(),
            html,
        )
        .unwrap();
        assert_eq!(jsonld[0].id, "https://example.test/story#photo");
        assert!(
            extract_media(
                MediaAdapter::JsonLd,
                &Url::parse("https://example.test/").unwrap(),
                &html.replace(",\"@id\":\"#photo\"", "")
            )
            .is_err()
        );
    }
}
