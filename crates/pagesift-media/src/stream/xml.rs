use std::collections::BTreeMap;

use anyhow::{Context, Result, bail, ensure};
use quick_xml::events::Event;
use url::Url;

use super::{Track, target};

#[derive(Default, Debug, Clone)]
pub(super) struct Xml {
    pub(super) name: String,
    pub(super) attrs: BTreeMap<String, String>,
    pub(super) children: Vec<Xml>,
    pub(super) text: String,
}
impl Xml {
    pub(super) fn child(&self, name: &str) -> Option<&Xml> {
        self.children.iter().find(|n| n.name == name)
    }
    pub(super) fn attr(&self, name: &str) -> Option<&str> {
        self.attrs.get(name).map(String::as_str)
    }
    pub(super) fn base(&self, base: &Url) -> Result<Url> {
        ensure!(
            self.children.iter().filter(|n| n.name == "BaseURL").count() <= 1,
            "multiple DASH BaseURL alternatives unsupported"
        );
        match self.child("BaseURL") {
            Some(n) => Ok(Url::parse(&target(base, n.text.trim())?)?),
            None => Ok(base.clone()),
        }
    }
}
pub(super) fn xml(source: &str) -> Result<Xml> {
    // Text is trimmed per element when it closes, not per event: an entity
    // splits text into several events and their inner spaces must survive.
    let mut reader = quick_xml::Reader::from_str(source);
    let mut stack = vec![Xml::default()];
    let mut count = 0;
    loop {
        match reader.read_event()? {
            Event::Start(e) | Event::Empty(e) => {
                count += 1;
                ensure!(
                    count <= 10_000 && stack.len() <= 32,
                    "DASH XML budget exceeded"
                );
                let mut n = Xml {
                    name: e.local_name().as_ref().to_owned(),
                    ..Default::default()
                };
                for a in e.attributes() {
                    let a = a?;
                    let key = a.key.as_ref().to_owned();
                    ensure!(
                        !key.ends_with(":href"),
                        "external DASH XML references unsupported"
                    );
                    n.attrs.insert(
                        key,
                        a.normalized_value(quick_xml::XmlVersion::Implicit1_0)?
                            .into_owned(),
                    );
                }
                // Empty events and start events share payload but not variant; the
                // source byte just consumed ends in '/>' only for an empty element.
                let end = reader.buffer_position() as usize;
                if source.as_bytes().get(end.saturating_sub(2)..end) == Some(b"/>") {
                    stack.last_mut().unwrap().children.push(n);
                } else {
                    stack.push(n);
                }
            }
            Event::End(_) => {
                ensure!(stack.len() > 1, "unexpected DASH close tag");
                let mut n = stack.pop().unwrap();
                let trimmed = n.text.trim();
                if trimmed.len() != n.text.len() {
                    n.text = trimmed.to_owned();
                }
                stack.last_mut().unwrap().children.push(n);
            }
            Event::Text(e) => stack.last_mut().unwrap().text.push_str(&e.xml10_content()),
            Event::GeneralRef(e) => {
                let value = quick_xml::escape::unescape(&format!("&{};", &*e))?.into_owned();
                stack.last_mut().unwrap().text.push_str(&value);
            }
            Event::DocType(_) => bail!("DASH DTD unsupported"),
            Event::Eof => break,
            Event::CData(_) => bail!("DASH CDATA unsupported"),
            _ => {}
        }
    }
    ensure!(
        stack.len() == 1 && stack[0].children.len() == 1,
        "invalid DASH document"
    );
    Ok(stack.pop().unwrap().children.remove(0))
}

/// Keep MPD timing and codec metadata, replace addressing with validated local
/// assets, and remove remote base/clock/location hints. No A/V remux is needed
/// to preserve the presentation window in a local DASH manifest.
pub(super) fn offline_dash(
    source: &str,
    tracks: &[Track],
    files: &[Vec<crate::download::DownloadResult>],
    directory: &std::path::Path,
    revision: &str,
) -> Result<String> {
    use std::fs::{self, File};
    use std::io::Read;
    let mut root = xml(source)?;
    // Xml stores local element names. Supply the DASH default namespace even
    // when the source used a prefix rather than xmlns on the root element.
    root.attrs
        .insert("xmlns".into(), "urn:mpeg:dash:schema:mpd:2011".into());
    let asset_name = format!("assets-{revision}");
    let assets = directory.join(&asset_name);
    match fs::symlink_metadata(&assets) {
        Ok(m) => ensure!(
            m.file_type().is_dir(),
            "DASH assets path must be a directory, not a symlink"
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => fs::create_dir(&assets)?,
        Err(e) => return Err(e.into()),
    }
    let mut index = 0;
    for period in root.children.iter_mut().filter(|n| n.name == "Period") {
        for set in period
            .children
            .iter_mut()
            .filter(|n| n.name == "AdaptationSet")
        {
            let parent_list = set.child("SegmentList").cloned();
            let parent_template = set.child("SegmentTemplate").cloned();
            for rep in set
                .children
                .iter_mut()
                .filter(|n| n.name == "Representation")
            {
                let track = tracks.get(index).context("DASH track mapping mismatch")?;
                let blobs = files.get(index).context("DASH download mapping mismatch")?;
                ensure!(
                    blobs.len() == track.segments.len(),
                    "DASH segment mapping mismatch"
                );
                let mut addressing = rep
                    .child("SegmentList")
                    .or(parent_list.as_ref())
                    .or_else(|| rep.child("SegmentTemplate"))
                    .or(parent_template.as_ref())
                    .context("DASH addressing missing")?
                    .clone();
                if addressing.name == "SegmentTemplate"
                    && let Some(parent) = &parent_template
                {
                    for (key, value) in &parent.attrs {
                        addressing
                            .attrs
                            .entry(key.clone())
                            .or_insert_with(|| value.clone());
                    }
                    if addressing.child("SegmentTimeline").is_none()
                        && let Some(timeline) = parent.child("SegmentTimeline")
                    {
                        addressing.children.push(timeline.clone());
                    }
                }
                let template_mode = addressing.name == "SegmentTemplate";
                let mut number: u64 = addressing.attr("startNumber").unwrap_or("1").parse()?;
                let mut list = Xml {
                    name: if template_mode {
                        "SegmentTemplate"
                    } else {
                        "SegmentList"
                    }
                    .into(),
                    attrs: addressing
                        .attrs
                        .into_iter()
                        .filter(|(k, _)| {
                            matches!(
                                k.as_str(),
                                "timescale" | "duration" | "presentationTimeOffset" | "startNumber"
                            )
                        })
                        .collect(),
                    children: addressing
                        .children
                        .into_iter()
                        .filter(|n| n.name == "SegmentTimeline")
                        .collect(),
                    ..Default::default()
                };
                for (si, (segment, blob)) in track.segments.iter().zip(blobs).enumerate() {
                    let ext = if segment.initialization {
                        "mp4"
                    } else if track.mime.ends_with("webm") {
                        "webm"
                    } else {
                        "m4s"
                    };
                    let filename = if template_mode && !segment.initialization {
                        let name = format!("t{index}-n{number}.{ext}");
                        number = number
                            .checked_add(1)
                            .context("local DASH number overflow")?;
                        list.attrs.insert(
                            "media".into(),
                            format!("{asset_name}/t{index}-n$Number$.{ext}"),
                        );
                        name
                    } else {
                        format!("t{index}-s{si}.{ext}")
                    };
                    let path = assets.join(&filename);
                    match fs::hard_link(&blob.path, &path) {
                        Ok(()) => {}
                        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                            let m = fs::symlink_metadata(&path)?;
                            ensure!(
                                m.file_type().is_file() && m.len() == blob.bytes,
                                "DASH asset collision"
                            );
                            let mut file = File::open(&path)?;
                            let mut hash = blake3::Hasher::new();
                            let mut buffer = [0; 64 * 1024];
                            let mut count = 0;
                            loop {
                                let n = file.read(&mut buffer)?;
                                if n == 0 {
                                    break;
                                }
                                count += n as u64;
                                ensure!(count <= blob.bytes, "DASH asset grew during verification");
                                hash.update(&buffer[..n]);
                            }
                            ensure!(
                                count == blob.bytes
                                    && hash.finalize().to_hex().as_str() == blob.blake3,
                                "DASH asset checksum mismatch"
                            );
                        }
                        Err(e) => return Err(e.into()),
                    }
                    if template_mode {
                        if segment.initialization {
                            list.attrs.insert(
                                "initialization".into(),
                                format!("{asset_name}/{filename}"),
                            );
                        }
                    } else {
                        list.children.push(Xml {
                            name: if segment.initialization {
                                "Initialization"
                            } else {
                                "SegmentURL"
                            }
                            .into(),
                            attrs: BTreeMap::from([(
                                if segment.initialization {
                                    "sourceURL"
                                } else {
                                    "media"
                                }
                                .into(),
                                format!("{asset_name}/{filename}"),
                            )]),
                            ..Default::default()
                        });
                    }
                }
                list.children.sort_by_key(|n| match n.name.as_str() {
                    "Initialization" => 0,
                    "SegmentTimeline" => 1,
                    _ => 2,
                });
                rep.children
                    .retain(|n| !matches!(n.name.as_str(), "SegmentList" | "SegmentTemplate"));
                rep.children.push(list);
                index += 1;
            }
            set.children
                .retain(|n| !matches!(n.name.as_str(), "SegmentList" | "SegmentTemplate"));
        }
    }
    ensure!(index == tracks.len(), "DASH representation count mismatch");
    #[cfg(unix)]
    File::open(&assets)?.sync_all()?;
    strip_remote_hints(&mut root);
    let mut result = String::from("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n");
    write_xml(&root, &mut result)?;
    Ok(result)
}
pub(super) fn strip_remote_hints(node: &mut Xml) {
    node.children.retain(|n| {
        !matches!(
            n.name.as_str(),
            "BaseURL" | "Location" | "PatchLocation" | "UTCTiming"
        )
    });
    for n in &mut node.children {
        strip_remote_hints(n);
    }
}
pub(super) fn write_xml(node: &Xml, out: &mut String) -> Result<()> {
    out.push('<');
    out.push_str(&node.name);
    for (k, v) in &node.attrs {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        out.push_str(&quick_xml::escape::escape(v));
        out.push('"');
    }
    if node.children.is_empty() && node.text.is_empty() {
        out.push_str("/>");
    } else {
        out.push('>');
        out.push_str(&quick_xml::escape::escape(&node.text));
        for n in &node.children {
            write_xml(n, out)?;
        }
        out.push_str("</");
        out.push_str(&node.name);
        out.push('>');
    }
    ensure!(
        out.len() <= 2 * 1024 * 1024,
        "local DASH manifest exceeds 2 MiB"
    );
    Ok(())
}
