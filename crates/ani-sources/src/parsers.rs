use std::collections::BTreeMap;

use ani_domain::{
    Release, ReleaseResolution, ReleaseSourceConfig, ReleaseSourceMeta, SubtitlePreference,
};
use chrono::{Local, SecondsFormat, TimeZone, Utc};
use data_encoding::BASE32_NOPAD;
use quick_xml::events::Event;
use quick_xml::{Reader, XmlVersion};
use regex::Regex;
use scraper::{ElementRef, Html, Selector};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use url::Url;

use crate::release::enrich_release_from_title;
use crate::SourceError;

const DEFAULT_DMHY_BASE_URL: &str = "https://share.dmhy.org/";
const DEFAULT_MIKAN_BASE_URL: &str = "https://mikanani.me/";
const DEFAULT_ACGNX_BASE_URL: &str = "https://share.acgnx.se/";
const MAX_RELEASE_ID_BYTES: usize = 200;

/// Mikan 番剧页中的字幕组订阅描述。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MikanSubgroup {
    pub id: String,
    pub name: String,
    pub rss_url: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct XmlNode {
    name: String,
    attributes: BTreeMap<String, String>,
    text: String,
    children: Vec<XmlNode>,
}

impl XmlNode {
    /// 返回第一个指定本地名称的直接子节点。
    fn child(&self, name: &str) -> Option<&Self> {
        self.children
            .iter()
            .find(|child| child.name.eq_ignore_ascii_case(name))
    }

    /// 返回全部指定本地名称的直接子节点。
    fn children_named<'node>(&'node self, name: &'node str) -> impl Iterator<Item = &'node Self> {
        self.children
            .iter()
            .filter(move |child| child.name.eq_ignore_ascii_case(name))
    }

    /// 读取直接子节点文本。
    fn child_text(&self, name: &str) -> Option<&str> {
        self.child(name).and_then(Self::text_value)
    }

    /// 读取当前节点去除空白后的文本。
    fn text_value(&self) -> Option<&str> {
        let value = self.text.trim();
        (!value.is_empty()).then_some(value)
    }

    /// 读取不区分大小写的属性。
    fn attribute(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// 解析通用 RSS、Nyaa 和 ACG.RIP 扩展字段。
pub fn parse_rss_releases(
    xml: &str,
    config: &ReleaseSourceConfig,
    rss_url: Option<&str>,
) -> Result<Vec<Release>, SourceError> {
    let root = parse_xml(xml)?;
    let items = rss_items(&root);
    Ok(items
        .into_iter()
        .enumerate()
        .map(|(index, item)| map_rss_item(item, index, config, rss_url))
        .map(|release| enrich_release_from_title(release, &[]))
        .collect())
}

/// 解析 Torznab RSS 的 enclosure、分页元数据和 attr 字段。
pub fn parse_torznab_releases(
    xml: &str,
    config: &ReleaseSourceConfig,
) -> Result<TorznabPage, SourceError> {
    let root = parse_xml(xml)?;
    let channel = root
        .child("rss")
        .and_then(|rss| rss.child("channel"))
        .or_else(|| root.child("channel"));
    let Some(channel) = channel else {
        return Ok(TorznabPage::default());
    };
    let releases = channel
        .children_named("item")
        .enumerate()
        .map(|(index, item)| {
            let title = item
                .child_text("title")
                .map(str::to_owned)
                .unwrap_or_else(|| format!("Torznab Item {}", index + 1));
            let enclosure = item.child("enclosure");
            let attrs = item.children_named("attr").collect::<Vec<_>>();
            let link = item
                .child_text("link")
                .or_else(|| enclosure.and_then(|node| node.attribute("url")))
                .map(str::to_owned);
            let size = enclosure
                .and_then(|node| node.attribute("length"))
                .and_then(parse_i64)
                .or_else(|| torznab_attr_number(&attrs, "size"));
            enrich_release_from_title(
                Release {
                    id: stable_release_id(
                        &config.id,
                        item.child_text("guid")
                            .or(link.as_deref())
                            .unwrap_or(&title),
                    ),
                    title,
                    magnet_url: link
                        .as_ref()
                        .filter(|value| value.starts_with("magnet:"))
                        .cloned(),
                    torrent_url: link.filter(|value| !value.starts_with("magnet:")),
                    size,
                    seeders: torznab_attr_number(&attrs, "seeders"),
                    published_at: item
                        .child_text("pubDate")
                        .map(str::to_owned)
                        .unwrap_or_else(now_iso),
                    source_id: config.id.clone(),
                    source_name: config.name.clone(),
                    ..empty_release()
                },
                &[],
            )
        })
        .collect();
    let response = channel.child("response");
    Ok(TorznabPage {
        releases,
        offset: response
            .and_then(|node| node.attribute("offset"))
            .and_then(parse_usize),
        total: response
            .and_then(|node| node.attribute("total"))
            .and_then(parse_usize),
    })
}

/// Torznab 单页资源和服务端分页信息。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct TorznabPage {
    pub releases: Vec<Release>,
    pub offset: Option<usize>,
    pub total: Option<usize>,
}

/// 解析 AniBT RSS 扩展字段与内嵌 torrent 元数据。
pub fn parse_anibt_rss(
    xml: &str,
    config: &ReleaseSourceConfig,
) -> Result<Vec<Release>, SourceError> {
    let root = parse_xml(xml)?;
    Ok(rss_items(&root)
        .into_iter()
        .enumerate()
        .map(|(index, item)| {
            let torrent = item.child("torrent");
            let title = item
                .child_text("releaseTitle")
                .or_else(|| torrent.and_then(|node| node.child_text("filename")))
                .or_else(|| item.child_text("title"))
                .map(str::to_owned)
                .unwrap_or_else(|| format!("AniBT Item {}", index + 1));
            let release_id = item
                .child_text("releaseId")
                .or_else(|| item.child_text("guid"));
            let magnet_url = torrent
                .and_then(|node| node.child_text("magneturi"))
                .map(str::to_owned)
                .or_else(|| item.child_text("description").and_then(find_magnet));
            let torrent_url = item
                .child_text("torrentUrl")
                .or_else(|| {
                    item.child("enclosure")
                        .and_then(|node| node.attribute("url"))
                })
                .map(str::to_owned);
            let info_hash = torrent
                .and_then(|node| node.child_text("infohash"))
                .map(|value| value.to_lowercase())
                .or_else(|| extract_info_hash(magnet_url.as_deref()));
            let custom_tags = item
                .children_named("customTag")
                .filter_map(XmlNode::text_value)
                .collect::<Vec<_>>();
            let declared_video_codec = custom_tags
                .into_iter()
                .find(|tag| is_codec_tag(tag))
                .map(str::to_owned);
            let subtitle = normalize_anibt_subtitle(item.child_text("language"));
            enrich_release_from_title(
                Release {
                    id: stable_release_id(
                        &config.id,
                        release_id
                            .or(info_hash.as_deref())
                            .or(torrent_url.as_deref())
                            .unwrap_or(&title),
                    ),
                    title,
                    fansub_name: item.child_text("groupName").map(str::to_owned),
                    magnet_url,
                    torrent_url,
                    info_hash,
                    size: item
                        .child_text("fileSize")
                        .and_then(parse_i64)
                        .or_else(|| {
                            torrent
                                .and_then(|node| node.child_text("contentLength"))
                                .and_then(parse_i64)
                        })
                        .or_else(|| {
                            item.child("enclosure")
                                .and_then(|node| node.attribute("length"))
                                .and_then(parse_i64)
                        }),
                    episode_no: item.child_text("episode").and_then(parse_f64),
                    resolution: normalize_resolution(item.child_text("resolution")),
                    declared_video_codec,
                    subtitle,
                    published_at: item
                        .child_text("pubDate")
                        .or_else(|| torrent.and_then(|node| node.child_text("pubDate")))
                        .map(str::to_owned)
                        .unwrap_or_else(now_iso),
                    source_id: config.id.clone(),
                    source_name: config.name.clone(),
                    ..empty_release()
                },
                &[],
            )
        })
        .collect())
}

/// 解析动漫花园列表页中的主题、磁链、种子和体积。
pub fn parse_dmhy_list(html: &str, config: &ReleaseSourceConfig) -> Vec<Release> {
    let document = Html::parse_document(html);
    select(&document, "tr")
        .filter_map(|row| map_dmhy_row(row, config))
        .map(|release| enrich_release_from_title(release, &[]))
        .collect()
}

/// 解析 Mikan 搜索页的资源行，并在只有 Episode 链接时生成 torrent 地址。
pub fn parse_mikan_release_list(html: &str, config: &ReleaseSourceConfig) -> Vec<Release> {
    let document = Html::parse_document(html);
    let rows = select(&document, "tr, li")
        .filter_map(|row| map_mikan_row(row, config))
        .collect::<Vec<_>>();
    let releases = if rows.is_empty() {
        select(&document, "a[href]")
            .filter_map(|anchor| map_mikan_anchor(anchor, config))
            .collect()
    } else {
        rows
    };
    releases
        .into_iter()
        .map(|release| enrich_release_from_title(release, &[]))
        .collect()
}

/// 解析 Mikan 番剧详情页的字幕组及其精确 RSS 地址。
pub fn parse_mikan_subgroups(
    html: &str,
    config: &ReleaseSourceConfig,
    source_anime_id: Option<&str>,
) -> Vec<MikanSubgroup> {
    let document = Html::parse_document(html);
    let bangumi_id = source_anime_id
        .map(str::to_owned)
        .or_else(|| parse_mikan_bangumi_id(&document));
    let mut groups = BTreeMap::<String, MikanSubgroup>::new();
    for element in select(&document, ".subgroup-name") {
        let id = element
            .value()
            .classes()
            .find_map(|class| class.strip_prefix("subgroup-"))
            .filter(|value| value.chars().all(|character| character.is_ascii_digit()));
        insert_mikan_group(
            &mut groups,
            id,
            element_text(element),
            config,
            bangumi_id.as_deref(),
        );
    }
    for element in select(&document, ".subgroup-text") {
        let id = element.value().id();
        let name = select_element(element, "a[href*='/Home/PublishGroup/']")
            .next()
            .map(element_text)
            .unwrap_or_default();
        insert_mikan_group(&mut groups, id, name, config, bangumi_id.as_deref());
    }
    groups.into_values().collect()
}

/// 解析 ACGNX JSON/API 风格响应。
pub fn parse_acgnx_api_response(payload: &Value, config: &ReleaseSourceConfig) -> Vec<Release> {
    find_record_array(payload)
        .into_iter()
        .flatten()
        .enumerate()
        .filter_map(|(index, value)| {
            value
                .as_object()
                .and_then(|record| map_acgnx_record(record, config, index))
        })
        .map(|release| enrich_release_from_title(release, &[]))
        .collect()
}

/// 解析 ACGNX HTML 搜索行中的下载地址、体积和做种数。
pub fn parse_acgnx_html(html: &str, config: &ReleaseSourceConfig) -> Vec<Release> {
    let document = Html::parse_document(html);
    select(&document, "tr, li")
        .filter_map(|row| map_acgnx_html_row(row, config))
        .map(|release| enrich_release_from_title(release, &[]))
        .collect()
}

fn parse_xml(xml: &str) -> Result<XmlNode, SourceError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(true);
    let mut stack = vec![XmlNode {
        name: "#document".to_owned(),
        ..XmlNode::default()
    }];
    loop {
        match reader.read_event() {
            Ok(Event::Start(event)) => stack.push(xml_node(&reader, &event)?),
            Ok(Event::Empty(event)) => {
                let node = xml_node(&reader, &event)?;
                stack
                    .last_mut()
                    .expect("XML document root must exist")
                    .children
                    .push(node);
            }
            Ok(Event::Text(event)) => {
                let decoded = event
                    .xml10_content()
                    .map_err(|error| SourceError::Parse(error.to_string()))?;
                let text = quick_xml::escape::unescape(&decoded)
                    .map_err(|error| SourceError::Parse(error.to_string()))?;
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&text);
                }
            }
            Ok(Event::CData(event)) => {
                let text = event
                    .xml10_content()
                    .map_err(|error| SourceError::Parse(error.to_string()))?;
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&text);
                }
            }
            Ok(Event::GeneralRef(event)) => {
                let reference = event
                    .xml10_content()
                    .map_err(|error| SourceError::Parse(error.to_string()))?;
                let escaped = format!("&{reference};");
                let text = quick_xml::escape::unescape(&escaped)
                    .map_err(|error| SourceError::Parse(error.to_string()))?;
                if let Some(node) = stack.last_mut() {
                    node.text.push_str(&text);
                }
            }
            Ok(Event::End(_)) => {
                let node = stack
                    .pop()
                    .ok_or_else(|| SourceError::Parse("XML 结束标签不匹配".to_owned()))?;
                stack
                    .last_mut()
                    .ok_or_else(|| SourceError::Parse("XML 缺少文档根节点".to_owned()))?
                    .children
                    .push(node);
            }
            Ok(Event::Eof) => break,
            Ok(_) => {}
            Err(error) => return Err(SourceError::Parse(error.to_string())),
        }
    }
    if stack.len() != 1 {
        return Err(SourceError::Parse("XML 存在未闭合标签".to_owned()));
    }
    stack
        .pop()
        .ok_or_else(|| SourceError::Parse("XML 文档为空".to_owned()))
}

fn xml_node(
    reader: &Reader<&[u8]>,
    event: &quick_xml::events::BytesStart<'_>,
) -> Result<XmlNode, SourceError> {
    let name = String::from_utf8_lossy(event.local_name().as_ref()).into_owned();
    let mut attributes = BTreeMap::new();
    for attribute in event.attributes().with_checks(false) {
        let attribute = attribute.map_err(|error| SourceError::Parse(error.to_string()))?;
        let key = String::from_utf8_lossy(attribute.key.local_name().as_ref()).into_owned();
        let value = attribute
            .decoded_and_normalized_value(XmlVersion::Implicit1_0, reader.decoder())
            .map_err(|error| SourceError::Parse(error.to_string()))?
            .into_owned();
        attributes.insert(key, value);
    }
    Ok(XmlNode {
        name,
        attributes,
        text: String::new(),
        children: Vec::new(),
    })
}

fn rss_items(root: &XmlNode) -> Vec<&XmlNode> {
    if let Some(channel) = root
        .child("rss")
        .and_then(|rss| rss.child("channel"))
        .or_else(|| root.child("channel"))
    {
        return channel.children_named("item").collect();
    }
    root.child("feed")
        .into_iter()
        .flat_map(|feed| feed.children_named("entry"))
        .collect()
}

fn map_rss_item(
    item: &XmlNode,
    index: usize,
    config: &ReleaseSourceConfig,
    rss_url: Option<&str>,
) -> Release {
    let title = item
        .child_text("title")
        .map(str::to_owned)
        .unwrap_or_else(|| format!("RSS Item {}", index + 1));
    let link = item
        .child("link")
        .and_then(|node| node.text_value().or_else(|| node.attribute("href")));
    let guid = item.child_text("guid");
    let enclosure = item.child("enclosure");
    let media = item
        .child("content")
        .filter(|node| node.attribute("url").is_some());
    let torrent = item.child("torrent");
    let enclosure_url = enclosure.and_then(|node| node.attribute("url"));
    let media_url = media.and_then(|node| node.attribute("url"));
    let torrent_link = torrent.and_then(|node| node.child_text("link"));
    let explicit_info_hash = normalize_info_hash(item.child_text("infoHash"));
    let feed_magnet = [link, enclosure_url, media_url, torrent_link]
        .into_iter()
        .flatten()
        .find(|value| value.starts_with("magnet:"));
    let info_hash = explicit_info_hash.or_else(|| extract_info_hash(feed_magnet));
    let magnet_url = feed_magnet
        .map(str::to_owned)
        .or_else(|| build_magnet_url(info_hash.as_deref(), &title));
    let torrent_url = [enclosure_url, media_url, torrent_link, link]
        .into_iter()
        .flatten()
        .find(|value| is_torrent_url(value))
        .map(str::to_owned);
    let info_hash = info_hash.or_else(|| extract_torrent_url_info_hash(torrent_url.as_deref()));
    let source_url = [guid, link]
        .into_iter()
        .flatten()
        .find(|value| is_source_page_url(value))
        .or(link)
        .or(guid)
        .map(str::to_owned);
    let source_meta = build_rss_source_meta(rss_url, source_url);
    let identity = guid
        .or(info_hash.as_deref())
        .or(magnet_url.as_deref())
        .or(torrent_url.as_deref())
        .or(link)
        .unwrap_or(&title);
    Release {
        id: stable_release_id(&config.id, identity),
        title,
        source_id: config.id.clone(),
        source_name: config.name.clone(),
        magnet_url,
        torrent_url,
        info_hash,
        size: [
            enclosure.and_then(|node| node.attribute("length")),
            torrent.and_then(|node| node.child_text("contentLength")),
            item.child_text("contentLength"),
            media.and_then(|node| node.attribute("fileSize")),
            item.child_text("size"),
        ]
        .into_iter()
        .flatten()
        .find_map(parse_byte_size),
        seeders: item.child_text("seeders").and_then(parse_i64),
        published_at: item
            .child_text("pubDate")
            .or_else(|| item.child_text("published"))
            .or_else(|| item.child_text("updated"))
            .or_else(|| torrent.and_then(|node| node.child_text("pubDate")))
            .map(str::to_owned)
            .unwrap_or_else(now_iso),
        source_meta,
        ..empty_release()
    }
}

/// 保留合法短标识，并为超长来源标识生成跨同步稳定的固定长度哈希。
fn stable_release_id(source_id: &str, identity: &str) -> String {
    let candidate = format!("{source_id}:{identity}");
    if !identity.trim().is_empty() && candidate.len() <= MAX_RELEASE_ID_BYTES {
        return candidate;
    }
    let mut digest = Sha256::new();
    digest.update(source_id.as_bytes());
    digest.update([0]);
    digest.update(identity.as_bytes());
    format!("release:{:x}", digest.finalize())
}

fn build_rss_source_meta(
    rss_url: Option<&str>,
    source_url: Option<String>,
) -> Option<ReleaseSourceMeta> {
    if rss_url.is_none() && source_url.is_none() {
        return None;
    }
    let mut metadata = ReleaseSourceMeta {
        source_url,
        rss_url: rss_url.map(str::to_owned),
        ..ReleaseSourceMeta::default()
    };
    if let Some(url) = rss_url.and_then(|value| Url::parse(value).ok()) {
        let mikan = url
            .host_str()
            .is_some_and(|host| host == "mikanani.me" || host.ends_with(".mikanani.me"))
            && url.path().to_lowercase().contains("/rss/bangumi");
        if mikan {
            metadata.mikan_bangumi_id = query_value(&url, "bangumiId");
            metadata.mikan_subgroup_id = query_value(&url, "subgroupid");
        }
    }
    Some(metadata)
}

fn map_dmhy_row(row: ElementRef<'_>, config: &ReleaseSourceConfig) -> Option<Release> {
    let anchors = select_element(row, "a[href]").collect::<Vec<_>>();
    let topic = anchors.iter().rev().find(|anchor| {
        anchor
            .value()
            .attr("href")
            .is_some_and(|href| href.contains("/topics/view/"))
    })?;
    let topic_href = topic.value().attr("href")?;
    let title = element_text(*topic);
    let magnet_url = find_anchor_href(&anchors, |href| href.starts_with("magnet:"));
    let torrent_url = find_anchor_href(&anchors, is_torrent_url).and_then(|href| {
        absolutize_url(
            &href,
            config.base_url.as_deref().unwrap_or(DEFAULT_DMHY_BASE_URL),
        )
    });
    if magnet_url.is_none() && torrent_url.is_none() {
        return None;
    }
    let info_hash = extract_info_hash(magnet_url.as_deref())
        .or_else(|| extract_torrent_url_info_hash(torrent_url.as_deref()));
    let topic_id = topic_href
        .split("/topics/view/")
        .nth(1)
        .and_then(|value| value.split(['/', '?', '#']).next());
    let text = element_text(row);
    Some(Release {
        id: stable_release_id(
            &config.id,
            info_hash.as_deref().or(topic_id).unwrap_or("unknown"),
        ),
        title,
        source_id: config.id.clone(),
        source_name: config.name.clone(),
        magnet_url,
        torrent_url,
        info_hash,
        size: parse_byte_size_from_text(&text),
        published_at: parse_html_datetime(&text).unwrap_or_else(now_iso),
        ..empty_release()
    })
}

fn map_mikan_row(row: ElementRef<'_>, config: &ReleaseSourceConfig) -> Option<Release> {
    let anchors = select_element(row, "a[href]").collect::<Vec<_>>();
    let episode = anchors.iter().find_map(|anchor| mikan_episode(*anchor));
    let title = episode.as_ref().map(|(_, title)| title.clone())?;
    let magnet_url = find_anchor_href(&anchors, |href| href.starts_with("magnet:"));
    let torrent = find_anchor_href(&anchors, is_torrent_url).or_else(|| {
        episode
            .as_ref()
            .map(|(id, _)| format!("/Download/{id}.torrent"))
    });
    if magnet_url.is_none() && torrent.is_none() {
        return None;
    }
    let torrent_url = torrent.and_then(|href| {
        absolutize_url(
            &href,
            config.base_url.as_deref().unwrap_or(DEFAULT_MIKAN_BASE_URL),
        )
    });
    let info_hash = extract_info_hash(magnet_url.as_deref())
        .or_else(|| extract_torrent_url_info_hash(torrent_url.as_deref()));
    let text = element_text(row);
    Some(Release {
        id: stable_release_id(
            &config.id,
            episode
                .as_ref()
                .map(|(id, _)| id.as_str())
                .or(info_hash.as_deref())
                .unwrap_or("unknown"),
        ),
        title,
        source_id: config.id.clone(),
        source_name: config.name.clone(),
        magnet_url,
        torrent_url,
        info_hash,
        size: parse_byte_size_from_text(&text),
        published_at: parse_html_datetime(&text).unwrap_or_else(now_iso),
        ..empty_release()
    })
}

fn map_mikan_anchor(anchor: ElementRef<'_>, config: &ReleaseSourceConfig) -> Option<Release> {
    let (id, title) = mikan_episode(anchor)?;
    let torrent_url = absolutize_url(
        &format!("/Download/{id}.torrent"),
        config.base_url.as_deref().unwrap_or(DEFAULT_MIKAN_BASE_URL),
    );
    let info_hash = extract_torrent_url_info_hash(torrent_url.as_deref());
    Some(Release {
        id: stable_release_id(&config.id, &id),
        title,
        source_id: config.id.clone(),
        source_name: config.name.clone(),
        torrent_url,
        info_hash,
        published_at: now_iso(),
        ..empty_release()
    })
}

fn mikan_episode(anchor: ElementRef<'_>) -> Option<(String, String)> {
    let href = anchor.value().attr("href")?;
    let id = href
        .split("/Home/Episode/")
        .nth(1)?
        .split(['/', '?', '#'])
        .next()?
        .trim();
    let title = element_text(anchor);
    (!id.is_empty() && title.chars().count() > 1).then(|| (id.to_owned(), title))
}

fn insert_mikan_group(
    groups: &mut BTreeMap<String, MikanSubgroup>,
    id: Option<&str>,
    name: String,
    config: &ReleaseSourceConfig,
    bangumi_id: Option<&str>,
) {
    let Some(id) = id.filter(|value| !value.is_empty()) else {
        return;
    };
    if name.trim().is_empty() || groups.contains_key(id) {
        return;
    }
    let mut url = Url::parse(config.base_url.as_deref().unwrap_or(DEFAULT_MIKAN_BASE_URL))
        .and_then(|base| base.join("/RSS/Bangumi"))
        .expect("Mikan 默认 URL 必须有效");
    if let Some(bangumi_id) = bangumi_id {
        url.query_pairs_mut().append_pair("bangumiId", bangumi_id);
    }
    url.query_pairs_mut().append_pair("subgroupid", id);
    groups.insert(
        id.to_owned(),
        MikanSubgroup {
            id: id.to_owned(),
            name,
            rss_url: url.to_string(),
        },
    );
}

fn parse_mikan_bangumi_id(document: &Html) -> Option<String> {
    select(document, "[data-bangumiid]")
        .find_map(|element| element.value().attr("data-bangumiid").map(str::to_owned))
        .or_else(|| {
            select(document, "a[href*='/RSS/Bangumi']").find_map(|anchor| {
                anchor
                    .value()
                    .attr("href")
                    .and_then(|href| Url::parse(&format!("https://mikanani.me{href}")).ok())
                    .and_then(|url| query_value(&url, "bangumiId"))
            })
        })
}

fn map_acgnx_record(
    record: &Map<String, Value>,
    config: &ReleaseSourceConfig,
    index: usize,
) -> Option<Release> {
    let title = get_string(
        record,
        &[
            "title",
            "name",
            "filename",
            "fileName",
            "resourceTitle",
            "torrentName",
            "subject",
        ],
    )?;
    let download = get_string(
        record,
        &[
            "magnet",
            "magnetUrl",
            "magnet_url",
            "magnetUri",
            "magnet_uri",
            "magnetLink",
            "magnet_link",
            "download",
            "downloadUrl",
            "download_url",
            "url",
        ],
    );
    let magnet_url = download
        .as_ref()
        .filter(|value| value.starts_with("magnet:"))
        .cloned();
    let torrent_value = get_string(
        record,
        &[
            "torrent",
            "torrentUrl",
            "torrent_url",
            "torrentLink",
            "torrent_link",
            "download",
            "downloadUrl",
            "download_url",
            "url",
        ],
    )
    .filter(|value| !value.starts_with("magnet:"));
    if magnet_url.is_none() && torrent_value.is_none() {
        return None;
    }
    let torrent_url = torrent_value.and_then(|value| {
        absolutize_url(
            &value,
            config.base_url.as_deref().unwrap_or(DEFAULT_ACGNX_BASE_URL),
        )
    });
    let info_hash = get_string(record, &["infoHash", "info_hash", "hash", "btih"])
        .map(|value| value.to_lowercase())
        .or_else(|| extract_info_hash(magnet_url.as_deref()));
    let id = get_string(
        record,
        &["id", "torrentId", "torrent_id", "resourceId", "resource_id"],
    );
    Some(Release {
        id: stable_release_id(
            &config.id,
            id.as_deref()
                .or(info_hash.as_deref())
                .or(magnet_url.as_deref())
                .or(torrent_url.as_deref())
                .unwrap_or(if index == 0 { "0" } else { "item" }),
        ),
        title,
        source_id: config.id.clone(),
        source_name: config.name.clone(),
        magnet_url,
        torrent_url,
        info_hash,
        size: get_size(record),
        seeders: get_number(
            record,
            &["seeders", "seeds", "seedCount", "seed_count", "seed"],
        ),
        published_at: get_string(
            record,
            &[
                "publishedAt",
                "published_at",
                "publishTime",
                "publish_time",
                "createdAt",
                "created_at",
                "date",
                "time",
            ],
        )
        .unwrap_or_else(now_iso),
        ..empty_release()
    })
}

fn map_acgnx_html_row(row: ElementRef<'_>, config: &ReleaseSourceConfig) -> Option<Release> {
    let anchors = select_element(row, "a[href]").collect::<Vec<_>>();
    let magnet_url = find_anchor_href(&anchors, |href| href.starts_with("magnet:"));
    let torrent_value = find_anchor_href(&anchors, is_torrent_url);
    if magnet_url.is_none() && torrent_value.is_none() {
        return None;
    }
    let title = anchors
        .iter()
        .rev()
        .find(|anchor| {
            anchor
                .value()
                .attr("href")
                .is_some_and(|href| !href.starts_with("magnet:") && !is_torrent_url(href))
        })
        .map(|anchor| element_text(*anchor))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| element_text(row));
    if title.is_empty() {
        return None;
    }
    let info_hash = extract_info_hash(magnet_url.as_deref());
    let torrent_url = torrent_value.and_then(|value| {
        absolutize_url(
            &value,
            config.base_url.as_deref().unwrap_or(DEFAULT_ACGNX_BASE_URL),
        )
    });
    let text = element_text(row);
    Some(Release {
        id: stable_release_id(
            &config.id,
            info_hash
                .as_deref()
                .or(torrent_url.as_deref())
                .unwrap_or("unknown"),
        ),
        title,
        source_id: config.id.clone(),
        source_name: config.name.clone(),
        magnet_url,
        torrent_url,
        info_hash,
        size: parse_byte_size_from_text(&text),
        seeders: parse_seeders(&text),
        published_at: parse_html_datetime(&text).unwrap_or_else(now_iso),
        ..empty_release()
    })
}

fn select<'document>(
    document: &'document Html,
    selector: &str,
) -> impl Iterator<Item = ElementRef<'document>> {
    let selector = Selector::parse(selector).expect("静态 HTML 选择器必须有效");
    document.select(&selector).collect::<Vec<_>>().into_iter()
}

fn select_element<'element>(
    element: ElementRef<'element>,
    selector: &str,
) -> impl Iterator<Item = ElementRef<'element>> {
    let selector = Selector::parse(selector).expect("静态 HTML 选择器必须有效");
    element.select(&selector).collect::<Vec<_>>().into_iter()
}

fn element_text(element: ElementRef<'_>) -> String {
    element
        .text()
        .flat_map(str::split_whitespace)
        .collect::<Vec<_>>()
        .join(" ")
}

fn find_anchor_href(
    anchors: &[ElementRef<'_>],
    predicate: impl Fn(&str) -> bool,
) -> Option<String> {
    anchors
        .iter()
        .filter_map(|anchor| anchor.value().attr("href"))
        .find(|href| predicate(href))
        .map(str::to_owned)
}

fn parse_html_datetime(text: &str) -> Option<String> {
    let pattern = Regex::new(r"\b(20\d{2})[/-](\d{1,2})[/-](\d{1,2})(?:\s+(\d{1,2}):(\d{1,2}))?\b")
        .expect("datetime regex");
    let captures = pattern.captures(text)?;
    let year = captures.get(1)?.as_str().parse().ok()?;
    let month = captures.get(2)?.as_str().parse().ok()?;
    let day = captures.get(3)?.as_str().parse().ok()?;
    let hour = captures
        .get(4)
        .map_or("0", |value| value.as_str())
        .parse()
        .ok()?;
    let minute = captures
        .get(5)
        .map_or("0", |value| value.as_str())
        .parse()
        .ok()?;
    Local
        .with_ymd_and_hms(year, month, day, hour, minute, 0)
        .single()
        .map(|value| {
            value
                .with_timezone(&Utc)
                .to_rfc3339_opts(SecondsFormat::Millis, true)
        })
}

fn parse_byte_size_from_text(text: &str) -> Option<i64> {
    let pattern =
        Regex::new(r"(?i)\b(\d+(?:\.\d+)?)\s*(TiB|GiB|MiB|KiB|TB|GB|MB|KB)\b").expect("size regex");
    let captures = pattern.captures(text)?;
    let value = captures.get(1)?.as_str().parse::<f64>().ok()?;
    let unit = captures.get(2)?.as_str().to_lowercase();
    let multiplier = match unit.as_str() {
        "kib" => 1_024_f64,
        "mib" => 1_024_f64.powi(2),
        "gib" => 1_024_f64.powi(3),
        "tib" => 1_024_f64.powi(4),
        "kb" => 1_000_f64,
        "mb" => 1_000_f64.powi(2),
        "gb" => 1_000_f64.powi(3),
        "tb" => 1_000_f64.powi(4),
        _ => return None,
    };
    Some((value * multiplier).round() as i64)
}

fn parse_byte_size(value: &str) -> Option<i64> {
    parse_i64(value).or_else(|| parse_byte_size_from_text(value.trim()))
}

fn parse_seeders(text: &str) -> Option<i64> {
    Regex::new(r"(?i)(?:seeders?|seeds?|做种|保种)\D{0,6}(\d{1,6})")
        .expect("seeders regex")
        .captures(text)
        .and_then(|captures| captures.get(1))
        .and_then(|value| parse_i64(value.as_str()))
}

pub(crate) fn normalize_info_hash(value: Option<&str>) -> Option<String> {
    let normalized = value?.trim();
    let normalized = normalized
        .get(..9)
        .filter(|prefix| prefix.eq_ignore_ascii_case("urn:btih:"))
        .and_then(|_| normalized.get(9..))
        .unwrap_or(normalized);
    if normalized.len() == 40 && normalized.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Some(normalized.to_ascii_lowercase());
    }
    if normalized.len() == 32 {
        if let Ok(bytes) = BASE32_NOPAD.decode(normalized.to_ascii_uppercase().as_bytes()) {
            if bytes.len() == 20 {
                return Some(bytes_to_hex(&bytes));
            }
        }
    }
    (normalized.len() >= 8
        && normalized.len() <= 64
        && normalized
            .chars()
            .all(|character| character.is_ascii_alphanumeric()))
    .then(|| normalized.to_ascii_lowercase())
}

pub(crate) fn extract_info_hash(magnet_url: Option<&str>) -> Option<String> {
    Regex::new(r"(?i)(?:^|[?&])xt=urn:btih:([a-z0-9]+)")
        .expect("info hash regex")
        .captures(magnet_url?)
        .and_then(|captures| captures.get(1))
        .and_then(|value| normalize_info_hash(Some(value.as_str())))
}

/// 从严格的 40 位十六进制 torrent 文件名提取 BTIH。
pub(crate) fn extract_torrent_url_info_hash(torrent_url: Option<&str>) -> Option<String> {
    let url = Url::parse(torrent_url?.trim()).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let file_name = url.path_segments()?.next_back()?;
    let hash = file_name.get(..40)?;
    let suffix = file_name.get(40..)?;
    (suffix.eq_ignore_ascii_case(".torrent")
        && hash.len() == 40
        && hash.bytes().all(|byte| byte.is_ascii_hexdigit()))
    .then(|| hash.to_ascii_lowercase())
}

/// 将种子摘要字节编码为稳定的小写十六进制。
fn bytes_to_hex(bytes: &[u8]) -> String {
    let mut result = String::with_capacity(bytes.len() * 2);
    const HEX: &[u8; 16] = b"0123456789abcdef";
    for byte in bytes {
        result.push(HEX[(byte >> 4) as usize] as char);
        result.push(HEX[(byte & 0x0f) as usize] as char);
    }
    result
}

fn build_magnet_url(info_hash: Option<&str>, title: &str) -> Option<String> {
    let info_hash = info_hash?;
    let mut url = Url::parse(&format!("magnet:?xt=urn:btih:{info_hash}")).ok()?;
    url.query_pairs_mut().append_pair("dn", title);
    Some(url.to_string())
}

fn find_magnet(value: &str) -> Option<String> {
    Regex::new(r#"(?i)magnet:\?[^\"' <]+"#)
        .expect("magnet regex")
        .find(value)
        .map(|matched| matched.as_str().replace("&amp;", "&"))
}

fn is_torrent_url(value: &str) -> bool {
    !value.starts_with("magnet:")
        && Regex::new(r"(?i)(?:\.torrent\b|/Download/|/download/|/topics/download/)")
            .expect("torrent URL regex")
            .is_match(value)
}

fn is_source_page_url(value: &str) -> bool {
    (value.starts_with("http://") || value.starts_with("https://")) && !is_torrent_url(value)
}

fn absolutize_url(value: &str, base_url: &str) -> Option<String> {
    Url::parse(value)
        .or_else(|_| Url::parse(base_url).and_then(|base| base.join(value)))
        .ok()
        .map(Into::into)
}

fn torznab_attr_number(attrs: &[&XmlNode], name: &str) -> Option<i64> {
    attrs
        .iter()
        .find(|node| node.attribute("name") == Some(name))
        .and_then(|node| node.attribute("value"))
        .and_then(parse_i64)
}

fn normalize_resolution(value: Option<&str>) -> Option<ReleaseResolution> {
    let value = value?.to_lowercase();
    if value.contains("2160p") || value.contains("4k") {
        Some(ReleaseResolution::P2160)
    } else if value.contains("1080p") {
        Some(ReleaseResolution::P1080)
    } else if value.contains("720p") {
        Some(ReleaseResolution::P720)
    } else {
        None
    }
}

fn normalize_anibt_subtitle(value: Option<&str>) -> Option<SubtitlePreference> {
    let value = value?.to_lowercase();
    if value.contains("chs") && value.contains("cht") {
        Some(SubtitlePreference::Multi)
    } else if value.contains("chs") || value.contains("sc") {
        Some(SubtitlePreference::Chs)
    } else if value.contains("cht") || value.contains("tc") {
        Some(SubtitlePreference::Cht)
    } else if value.contains("jpn") || value.contains("jp") {
        Some(SubtitlePreference::Jpn)
    } else if value.contains("eng") || value.contains("en") {
        Some(SubtitlePreference::Eng)
    } else {
        None
    }
}

fn is_codec_tag(value: &str) -> bool {
    Regex::new(r"(?i)\b(?:avc|h\.?264|x264|hevc|h\.?265|x265|av1|vp9)\b")
        .expect("codec tag regex")
        .is_match(value)
}

fn find_record_array(value: &Value) -> Option<&Vec<Value>> {
    if let Value::Array(values) = value {
        return Some(values);
    }
    let record = value.as_object()?;
    ["data", "items", "results", "list", "torrents", "resources"]
        .into_iter()
        .find_map(|key| record.get(key).and_then(find_record_array))
}

fn get_string(record: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match record.get(*key) {
        Some(Value::String(value)) if !value.trim().is_empty() => Some(value.trim().to_owned()),
        Some(Value::Number(value)) => Some(value.to_string()),
        _ => None,
    })
}

fn get_number(record: &Map<String, Value>, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| match record.get(*key) {
        Some(Value::Number(value)) => value.as_i64(),
        Some(Value::String(value)) => parse_i64(value),
        _ => None,
    })
}

fn get_size(record: &Map<String, Value>) -> Option<i64> {
    ["size", "fileSize", "file_size", "length", "bytes"]
        .into_iter()
        .find_map(|key| match record.get(key) {
            Some(Value::Number(value)) => value.as_i64(),
            Some(Value::String(value)) => parse_byte_size(value),
            _ => None,
        })
}

fn parse_i64(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok()
}

fn parse_usize(value: &str) -> Option<usize> {
    value.trim().parse::<usize>().ok()
}

fn parse_f64(value: &str) -> Option<f64> {
    value
        .trim()
        .parse::<f64>()
        .ok()
        .filter(|value| value.is_finite())
}

fn query_value(url: &Url, name: &str) -> Option<String> {
    url.query_pairs()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.into_owned())
}

fn now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn empty_release() -> Release {
    Release {
        id: String::new(),
        title: String::new(),
        anime_id: None,
        episode_no: None,
        episode_range: None,
        series_season_no: None,
        content_kind: None,
        fansub_group_id: None,
        fansub_name: None,
        source_id: String::new(),
        source_name: String::new(),
        magnet_url: None,
        torrent_url: None,
        info_hash: None,
        size: None,
        resolution: None,
        declared_video_codec: None,
        normalized_video_codec: None,
        bit_depth: None,
        subtitle_languages: Vec::new(),
        subtitle: None,
        published_at: String::new(),
        seeders: None,
        source_meta: None,
    }
}

#[cfg(test)]
mod tests {
    use ani_domain::{
        NormalizedVideoCodec, ReleaseResolution, ReleaseSourceConfig, SourceKind, SubtitleLanguage,
        SubtitlePreference,
    };
    use serde_json::json;

    use super::{
        extract_torrent_url_info_hash, parse_acgnx_api_response, parse_acgnx_html, parse_anibt_rss,
        parse_dmhy_list, parse_mikan_release_list, parse_mikan_subgroups, parse_rss_releases,
        parse_torznab_releases,
    };

    /// 验证 HTML 站点适配器输出统一资源字段。
    #[test]
    fn parses_dmhy_mikan_and_acgnx_html() {
        let dmhy = source("dmhy", "动漫花园", "https://share.dmhy.org/");
        let releases = parse_dmhy_list(
            r#"<table><tr><td>2026/07/13 12:30</td><td><a href="/topics/view/123.html">[喵萌奶茶屋] 测试番 - 01 [1080p][HEVC][简日]</a></td><td><a href="magnet:?xt=urn:btih:ABCDEF1234567890">磁力</a></td><td><a href="/topics/download/123.torrent">下载</a></td><td>1.25 GiB</td></tr></table>"#,
            &dmhy,
        );
        assert_eq!(releases.len(), 1);
        assert_eq!(releases[0].episode_no, Some(1.0));
        assert_eq!(
            releases[0].normalized_video_codec,
            Some(NormalizedVideoCodec::H265Hevc)
        );
        assert_eq!(releases[0].size, Some(1_342_177_280));

        let mikan = source("mikan-site", "蜜柑计划", "https://mikanani.me/");
        let releases = parse_mikan_release_list(
            r#"<table><tr><td><a href="/Home/Episode/456">[桜都字幕组] 测试番 - 02 [1080p][AVC][简体]</a></td><td><a href="magnet:?xt=urn:btih:1234ABCD">磁力</a></td><td>512.5 MB</td></tr></table>"#,
            &mikan,
        );
        assert_eq!(releases[0].id, "mikan-site:456");
        assert_eq!(
            releases[0].torrent_url.as_deref(),
            Some("https://mikanani.me/Download/456.torrent")
        );
        assert_eq!(releases[0].resolution, Some(ReleaseResolution::P1080));

        let mikan_hash = "1a58b0c190ad7d33688cf570fc3c6c05f983977c";
        let releases = parse_mikan_release_list(
            &format!(
                r#"<table><tr><td><a href="/Home/Episode/{mikan_hash}">[Nix-Raws] LV999的村民 S01E06 [1080p]</a></td></tr></table>"#
            ),
            &mikan,
        );
        assert_eq!(releases[0].info_hash.as_deref(), Some(mikan_hash));
        assert_eq!(
            releases[0].torrent_url.as_deref(),
            Some("https://mikanani.me/Download/1a58b0c190ad7d33688cf570fc3c6c05f983977c.torrent")
        );
        assert_eq!(
            extract_torrent_url_info_hash(Some("https://mikanani.me/Download/123.torrent")),
            None
        );
        assert_eq!(
            extract_torrent_url_info_hash(Some(
                "https://example.test/file.torrent?hash=1a58b0c190ad7d33688cf570fc3c6c05f983977c"
            )),
            None
        );

        let acgnx = source("acgnx", "ACGNX", "https://share.acgnx.se/");
        let releases = parse_acgnx_html(
            r#"<table><tr><td><a href="/show-42.html">[Sakurato] 测试番 - 04 [720p][AVC]</a></td><td><a href="magnet:?xt=urn:btih:ABC123DEF456">磁力</a></td><td><a href="/download/42.torrent">下载</a></td><td>850 MB seeders 9</td></tr></table>"#,
            &acgnx,
        );
        assert_eq!(releases[0].seeders, Some(9));
        assert_eq!(releases[0].resolution, Some(ReleaseResolution::P720));
    }

    /// 验证通用 RSS、Torznab 和 AniBT 扩展字段解析。
    #[test]
    fn parses_rss_torznab_and_anibt_xml() {
        let rss = source("rss", "RSS", "https://example.test/");
        let releases = parse_rss_releases(
            r#"<rss xmlns:nyaa="https://nyaa.si/xmlns/nyaa"><channel><item><title>[组] 测试番 - 38 [1080p][简繁]</title><link>https://nyaa.si/download/1.torrent</link><guid>https://nyaa.si/view/1</guid><nyaa:seeders>16</nyaa:seeders><nyaa:infoHash>1188285F8B296E1E7E2F622955F214B71E93D2DC</nyaa:infoHash><nyaa:size>663.2 MiB</nyaa:size></item></channel></rss>"#,
            &rss,
            Some("https://nyaa.si/?page=rss"),
        )
        .expect("parse RSS");
        assert_eq!(releases[0].id, "rss:https://nyaa.si/view/1");
        assert_eq!(releases[0].seeders, Some(16));
        assert_eq!(releases[0].subtitle, Some(SubtitlePreference::Multi));
        assert_eq!(
            releases[0].subtitle_languages,
            vec![SubtitleLanguage::Chs, SubtitleLanguage::Cht]
        );

        let mikan_hash = "1a58b0c190ad7d33688cf570fc3c6c05f983977c";
        let mikan_rss = source("mikan", "蜜柑计划 RSS", "https://mikanani.me/");
        let releases = parse_rss_releases(
            &format!(
                r#"<rss><channel><item><title>[Nix-Raws] LV999的村民 S01E06 [1080p]</title><link>https://mikanani.me/Home/Episode/{mikan_hash}</link><enclosure url="https://mikanani.me/Download/20260730/{mikan_hash}.torrent"/></item></channel></rss>"#
            ),
            &mikan_rss,
            Some("https://mikanani.me/RSS/Bangumi"),
        )
        .expect("parse Mikan RSS");
        assert_eq!(releases[0].info_hash.as_deref(), Some(mikan_hash));

        let torznab = source("torznab", "Torznab", "https://indexer.test/");
        let page = parse_torznab_releases(
            r#"<rss xmlns:torznab="x" xmlns:newznab="y"><channel><newznab:response offset="0" total="1"/><item><title>[组] 测试番 - 05 [2160p][AV1]</title><guid>item-5</guid><enclosure url="https://indexer.test/5.torrent" length="3221225472"/><torznab:attr name="seeders" value="42"/></item></channel></rss>"#,
            &torznab,
        )
        .expect("parse Torznab");
        assert_eq!(page.total, Some(1));
        assert_eq!(page.releases[0].seeders, Some(42));

        let anibt = source("anibt", "AniBT", "https://anibt.net/");
        let releases = parse_anibt_rss(
            r#"<rss xmlns:anibt="x"><channel><item><anibt:releaseId>rel-1</anibt:releaseId><anibt:releaseTitle>[Nix-Raws] 测试番 S02E02 [1080p AVC][简繁]</anibt:releaseTitle><anibt:groupName>Nix-Raws</anibt:groupName><anibt:episode>2</anibt:episode><anibt:language>CHS/CHT</anibt:language><torrent><infohash>A307AE8DBE4B93226197A7D560651457AC9A28D4</infohash><magneturi>magnet:?xt=urn:btih:a307ae8dbe4b93226197a7d560651457ac9a28d4&amp;dn=test&amp;xl=1479404657&amp;tr=https%3A%2F%2Ftracker.anibt.net%2Fannounce</magneturi></torrent></item></channel></rss>"#,
            &anibt,
        )
        .expect("parse AniBT");
        assert_eq!(releases[0].id, "anibt:rel-1");
        assert_eq!(releases[0].fansub_name.as_deref(), Some("Nix-Raws"));
        assert_eq!(releases[0].episode_no, Some(2.0));
        assert_eq!(
            releases[0].magnet_url.as_deref(),
            Some(
                "magnet:?xt=urn:btih:a307ae8dbe4b93226197a7d560651457ac9a28d4&dn=test&xl=1479404657&tr=https%3A%2F%2Ftracker.anibt.net%2Fannounce"
            )
        );
    }

    /// 验证超长 RSS GUID 会生成稳定且可持久化的资源标识。
    #[test]
    fn hashes_oversized_rss_release_identifiers() {
        let rss = source(
            "rss-subscription:12345678-1234-1234-1234-123456789012",
            "Mikan RSS",
            "https://mikanani.me/",
        );
        let magnet = format!(
            "magnet:?xt=urn:btih:1188285F8B296E1E7E2F622955F214B71E93D2DC&dn={}",
            "long-title-".repeat(30)
        );
        let xml = format!(
            "<rss><channel><item><title>[组] 测试番 - 01 [简体]</title><guid>{0}</guid><link>{0}</link></item></channel></rss>",
            magnet.replace('&', "&amp;")
        );

        let first = parse_rss_releases(&xml, &rss, Some("https://mikanani.me/RSS/Bangumi"))
            .expect("parse oversized RSS");
        let second = parse_rss_releases(&xml, &rss, Some("https://mikanani.me/RSS/Bangumi"))
            .expect("parse oversized RSS again");

        assert_eq!(first[0].id, second[0].id);
        assert!(first[0].id.starts_with("release:"));
        assert!(first[0].id.len() <= 200);
    }

    /// 验证缺少 GUID、链接和下载地址的 RSS 条目仍会生成非空稳定标识。
    #[test]
    fn generates_id_for_sparse_rss_items() {
        let rss = source("mikan", "蜜柑计划 RSS", "https://mikanani.me/");
        let xml = "<rss><channel><item /></channel></rss>";

        let first = parse_rss_releases(xml, &rss, Some("https://mikanani.me/RSS/Bangumi"))
            .expect("parse sparse RSS");
        let second = parse_rss_releases(xml, &rss, Some("https://mikanani.me/RSS/Bangumi"))
            .expect("parse sparse RSS again");

        assert_eq!(first[0].id, "mikan:RSS Item 1");
        assert_eq!(first[0].id, second[0].id);
        assert!(!first[0].id.trim().is_empty());
    }

    /// 验证 AniBT 的超长 releaseId 同样生成稳定且可持久化的资源标识。
    #[test]
    fn hashes_oversized_anibt_release_identifiers() {
        let anibt = source("anibt", "AniBT", "https://anibt.net/");
        let release_id = "oversized-release-id-".repeat(16);
        let xml = format!(
            "<rss xmlns:anibt=\"x\"><channel><item><anibt:releaseId>{release_id}</anibt:releaseId><anibt:releaseTitle>[组] 测试番 - 01 [1080p]</anibt:releaseTitle></item></channel></rss>"
        );

        let first = parse_anibt_rss(&xml, &anibt).expect("parse oversized AniBT release");
        let second = parse_anibt_rss(&xml, &anibt).expect("parse oversized AniBT release again");

        assert_eq!(first[0].id, second[0].id);
        assert!(first[0].id.starts_with("release:"));
        assert!(first[0].id.len() <= 200);
    }

    /// 验证 Mikan 字幕组和 ACGNX JSON 映射。
    #[test]
    fn parses_mikan_subgroups_and_acgnx_json() {
        let mikan = source("mikan", "Mikan", "https://mikanani.me/");
        let groups = parse_mikan_subgroups(
            r#"<a class="subgroup-name subgroup-370">LoliHouse</a><div class="subgroup-text" id="382"><a href="/Home/PublishGroup/233">喵萌奶茶屋</a></div>"#,
            &mikan,
            Some("3941"),
        );
        assert_eq!(groups.len(), 2);
        assert!(groups[0].rss_url.contains("bangumiId=3941"));

        let acgnx = source("acgnx", "ACGNX", "https://share.acgnx.se/");
        let releases = parse_acgnx_api_response(
            &json!({"data":{"items":[{"id":"100","title":"[组] 测试番 - 03 [1080p][HEVC]","magnet":"magnet:?xt=urn:btih:FACEB00C1234","size":"1.50 GiB","seeders":"18"}]}}),
            &acgnx,
        );
        assert_eq!(releases[0].id, "acgnx:100");
        assert_eq!(releases[0].size, Some(1_610_612_736));
    }

    fn source(id: &str, name: &str, base_url: &str) -> ReleaseSourceConfig {
        ReleaseSourceConfig {
            id: id.to_owned(),
            name: name.to_owned(),
            kind: SourceKind::SiteAdapter,
            enabled: true,
            use_proxy: false,
            request_interval_ms: 250,
            base_url: Some(base_url.to_owned()),
            api_key: None,
            rss_url: None,
            tags: Vec::new(),
        }
    }
}
