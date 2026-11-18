//! Store listing rules shared by `vesper store check`, publishing and catalogue aggregation.
//!
//! `store.json` is developer content, so every field is checked here before a
//! catalogue copies it. The rules match `index-template/schemas/store.schema.json`
//! and `index-template/scripts/validate-store.mjs`; the fixtures in
//! `index-template/test/fixtures/store/` are run through all of them.
use serde_json::{Map, Value};

/// Files every listing signs.
pub const REQUIRED_FILES: [&str; 2] = ["store.json", "description.md"];
/// Upper bound on signed store files (store.json, descriptions, changelogs).
pub const MAX_FILES: usize = 32;
/// Upper bound on one Markdown file (description or changelog).
pub const MAX_MARKDOWN_BYTES: usize = 64 * 1024;

pub const PLATFORMS: [&str; 5] = [
    "windows-x64",
    "linux-x64",
    "macos-arm64",
    "macos-x64",
    "web-wasm32",
];
pub const RELEASE_STATUSES: [&str; 3] = ["coming-soon", "early-access", "released"];
pub const FEATURES: [&str; 12] = [
    "single-player",
    "online-co-op",
    "local-co-op",
    "online-pvp",
    "local-pvp",
    "controller",
    "keyboard-mouse",
    "touch",
    "cloud-saves",
    "achievements",
    "mod-support",
    "level-editor",
];
pub const CONTENT_DESCRIPTORS: [&str; 10] = [
    "violence",
    "blood",
    "gore",
    "strong-language",
    "sexual-content",
    "nudity",
    "drugs",
    "alcohol",
    "gambling",
    "horror",
];
pub const ACCESSIBILITY: [&str; 9] = [
    "subtitles",
    "colorblind-modes",
    "remappable-controls",
    "adjustable-text-size",
    "screen-reader",
    "one-handed",
    "difficulty-options",
    "reduced-motion",
    "no-flashing-lights",
];
pub const LINKS: [&str; 12] = [
    "discord",
    "x",
    "bluesky",
    "youtube",
    "twitch",
    "reddit",
    "steam",
    "itch",
    "github",
    "wiki",
    "press-kit",
    "support",
];
pub const REQUIREMENT_KEYS: [&str; 7] =
    ["os", "cpu", "gpu", "memory", "storage", "network", "notes"];

const REQUIRED: [&str; 11] = [
    "title",
    "summary",
    "tags",
    "screenshots",
    "cover",
    "requirements",
    "paid",
    "price_display",
    "developer",
    "homepage",
    "platforms",
];
const OPTIONAL: [&str; 15] = [
    "default_language",
    "release",
    "features",
    "max_players",
    "languages",
    "system_requirements",
    "capsule",
    "logo",
    "background",
    "trailers",
    "links",
    "publishers",
    "content_descriptors",
    "accessibility",
    "updated",
];

/// A BCP 47 style language tag: `en`, `zh-Hans`, `pt-BR`, `es-419`.
pub fn is_language_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let primary = parts.next().unwrap_or("");
    tag.len() <= 35
        && (2..=3).contains(&primary.len())
        && primary.bytes().all(|b| b.is_ascii_alphabetic())
        && parts.all(|p| (1..=8).contains(&p.len()) && p.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Signed store file names: `store.json`, `description.md`, `changelog.md`,
/// and per-language `description.<lang>.md` / `changelog.<lang>.md`.
pub fn is_store_file_name(name: &str) -> bool {
    if REQUIRED_FILES.contains(&name) || name == "changelog.md" {
        return true;
    }
    let Some(stem) = name.strip_suffix(".md") else {
        return false;
    };
    ["description.", "changelog."]
        .iter()
        .any(|prefix| stem.strip_prefix(prefix).is_some_and(is_language_tag))
}

/// Problems with a `store.json` value; empty when it is valid.
pub fn validate(store: &Value) -> Vec<String> {
    let mut errors = Vec::new();
    let mut fail = |message: String| errors.push(message);
    let Some(store) = store.as_object() else {
        return vec!["store.json must be an object".into()];
    };
    for key in REQUIRED {
        if !store.contains_key(key) {
            fail(format!("missing {key}"));
        }
    }
    for key in store.keys() {
        if !REQUIRED.contains(&key.as_str()) && !OPTIONAL.contains(&key.as_str()) {
            fail(format!("unknown field {key}"));
        }
    }
    for key in ["title", "summary"] {
        if let Some(value) = store.get(key) {
            if !language_map(value, 300) {
                fail(format!("{key} must map language tags to non-empty text"));
            }
        }
    }
    if let Some(tags) = store.get("tags") {
        let ok = tags.as_array().is_some_and(|tags| {
            (1..=20).contains(&tags.len())
                && tags.iter().all(|tag| tag.as_str().is_some_and(is_tag))
        });
        if !ok {
            fail("tags must be 1 to 20 lowercase slugs (a-z, 0-9, -)".into());
        }
    }
    if let Some(shots) = store.get("screenshots") {
        match shots.as_array() {
            Some(list) if list.len() <= 30 => {
                for (i, item) in list.iter().enumerate() {
                    if !media(item) {
                        fail(format!("screenshots[{i}] {MEDIA_RULE}"));
                    }
                }
            }
            _ => fail("screenshots must be a list of at most 30 images".into()),
        }
    }
    for key in ["cover", "capsule", "logo", "background"] {
        if store.get(key).is_some_and(|value| !media(value)) {
            fail(format!("{key} {MEDIA_RULE}"));
        }
    }
    if let Some(value) = store.get("requirements") {
        if !value.as_object().is_some_and(|map| {
            map.values()
                .all(|v| v.as_str().is_some_and(|s| s.len() <= 300))
        }) {
            fail("requirements must map names to text".into());
        }
    }
    if store.get("paid").is_some_and(|v| !v.is_boolean()) {
        fail("paid must be true or false".into());
    }
    if store.get("price_display").is_some_and(|v| !v.is_string()) {
        fail("price_display must be text".into());
    }
    if store.get("developer").is_some_and(|v| !text(v, 100)) {
        fail("developer must be non-empty text".into());
    }
    if store.get("homepage").is_some_and(|v| !https(v)) {
        fail("homepage must be an https:// URL".into());
    }
    if let Some(value) = store.get("platforms") {
        if !enum_list(value, &PLATFORMS, true) {
            fail(format!(
                "platforms must list some of {}",
                PLATFORMS.join(", ")
            ));
        }
    }
    if let Some(value) = store.get("default_language") {
        if !value.as_str().is_some_and(is_language_tag) {
            fail("default_language must be a language tag such as en or zh-Hans".into());
        }
    }
    if let Some(value) = store.get("release") {
        let ok = value.as_object().is_some_and(|release| {
            only_keys(release, &["status", "date"])
                && release
                    .get("status")
                    .and_then(Value::as_str)
                    .is_some_and(|s| RELEASE_STATUSES.contains(&s))
                && release
                    .get("date")
                    .is_none_or(|d| d.as_str().is_some_and(is_date))
        });
        if !ok {
            fail(format!(
                "release needs status ({}) and an optional date YYYY-MM-DD",
                RELEASE_STATUSES.join(", ")
            ));
        }
    }
    if let Some(value) = store.get("updated") {
        if !value.as_str().is_some_and(is_date) {
            fail("updated must be a date YYYY-MM-DD".into());
        }
    }
    for (key, allowed) in [
        ("features", &FEATURES[..]),
        ("content_descriptors", &CONTENT_DESCRIPTORS[..]),
        ("accessibility", &ACCESSIBILITY[..]),
    ] {
        if let Some(value) = store.get(key) {
            if !enum_list(value, allowed, false) {
                fail(format!("{key} must list some of {}", allowed.join(", ")));
            }
        }
    }
    if let Some(value) = store.get("max_players") {
        if !value.as_u64().is_some_and(|n| (1..=1000).contains(&n)) {
            fail("max_players must be a whole number from 1 to 1000".into());
        }
    }
    if let Some(value) = store.get("languages") {
        let ok = value.as_object().is_some_and(|map| {
            !map.is_empty()
                && map.len() <= 100
                && map.iter().all(|(tag, support)| {
                    is_language_tag(tag)
                        && support.as_object().is_some_and(|s| {
                            only_keys(s, &["interface", "audio", "subtitles"])
                                && s.values().all(Value::is_boolean)
                                && s.values().any(|v| v == &Value::Bool(true))
                        })
                })
        });
        if !ok {
            fail("languages must map language tags to {interface, audio, subtitles} with at least one true".into());
        }
    }
    if let Some(value) = store.get("system_requirements") {
        let ok = value.as_object().is_some_and(|map| {
            map.iter().all(|(platform, tiers)| {
                PLATFORMS.contains(&platform.as_str())
                    && tiers.as_object().is_some_and(|tiers| {
                        !tiers.is_empty()
                            && only_keys(tiers, &["minimum", "recommended"])
                            && tiers.values().all(|spec| {
                                spec.as_object().is_some_and(|spec| {
                                    !spec.is_empty()
                                        && only_keys(spec, &REQUIREMENT_KEYS)
                                        && spec.values().all(|v| text(v, 200))
                                })
                            })
                    })
            })
        });
        if !ok {
            fail(format!(
                "system_requirements must map platforms to minimum/recommended with {}",
                REQUIREMENT_KEYS.join(", ")
            ));
        }
    }
    if let Some(value) = store.get("trailers") {
        match value.as_array() {
            Some(list) if list.len() <= 10 => {
                for (i, item) in list.iter().enumerate() {
                    if !trailer(item) {
                        fail(format!(
                            "trailers[{i}] must be {{kind: youtube, id}} or {{kind: video, url (.mp4/.webm), poster}}, with an optional title map"
                        ));
                    }
                }
            }
            _ => fail("trailers must be a list of at most 10 videos".into()),
        }
    }
    if let Some(value) = store.get("links") {
        let ok = value.as_object().is_some_and(|map| {
            map.iter()
                .all(|(key, url)| LINKS.contains(&key.as_str()) && https(url))
        });
        if !ok {
            fail(format!(
                "links must map {} to https:// URLs",
                LINKS.join(", ")
            ));
        }
    }
    if let Some(value) = store.get("publishers") {
        if !value
            .as_array()
            .is_some_and(|list| (1..=10).contains(&list.len()) && list.iter().all(|v| text(v, 100)))
        {
            fail("publishers must list 1 to 10 names".into());
        }
    }
    errors
}

const MEDIA_RULE: &str =
    "must have an https url, a 64-character blake3 and width/height from 1 to 16384";

fn text(value: &Value, max: usize) -> bool {
    value
        .as_str()
        .is_some_and(|s| !s.trim().is_empty() && s.chars().count() <= max)
}

fn https(value: &Value) -> bool {
    value.as_str().is_some_and(|s| {
        s.starts_with("https://")
            && s.len() > 8
            && s.len() <= 2048
            && !s.contains(char::is_whitespace)
    })
}

/// A catalogue tag: lowercase letters, digits and dashes, at most 32 bytes.
pub fn is_tag(tag: &str) -> bool {
    !tag.is_empty()
        && tag.len() <= 32
        && tag
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        && tag
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

fn is_date(value: &str) -> bool {
    let parts: Vec<_> = value.split('-').collect();
    let [y, m, d] = parts.as_slice() else {
        return false;
    };
    let number = |s: &str, len: usize| {
        (s.len() == len && s.bytes().all(|b| b.is_ascii_digit()))
            .then(|| s.parse::<u32>().ok())
            .flatten()
    };
    matches!(
        (number(y, 4), number(m, 2), number(d, 2)),
        (Some(1970..=2999), Some(1..=12), Some(1..=31))
    )
}

fn only_keys(map: &Map<String, Value>, allowed: &[&str]) -> bool {
    map.keys().all(|k| allowed.contains(&k.as_str()))
}

fn language_map(value: &Value, max: usize) -> bool {
    value.as_object().is_some_and(|map| {
        !map.is_empty()
            && map
                .iter()
                .all(|(tag, v)| is_language_tag(tag) && text(v, max))
    })
}

fn enum_list(value: &Value, allowed: &[&str], non_empty: bool) -> bool {
    value.as_array().is_some_and(|list| {
        (!non_empty || !list.is_empty())
            && list.iter().enumerate().all(|(i, v)| {
                v.as_str().is_some_and(|s| allowed.contains(&s)) && !list[..i].contains(v)
            })
    })
}

fn media(value: &Value) -> bool {
    value.as_object().is_some_and(|m| {
        only_keys(m, &["url", "blake3", "width", "height"])
            && m.get("url").is_some_and(https)
            && m.get("blake3")
                .and_then(Value::as_str)
                .is_some_and(|h| h.len() == 64 && h.bytes().all(|b| b.is_ascii_hexdigit()))
            && ["width", "height"].iter().all(|k| {
                m.get(*k)
                    .and_then(Value::as_u64)
                    .is_some_and(|n| (1..=16384).contains(&n))
            })
    })
}

fn trailer(value: &Value) -> bool {
    let Some(t) = value.as_object() else {
        return false;
    };
    let title_ok = t.get("title").is_none_or(|v| language_map(v, 100));
    match t.get("kind").and_then(Value::as_str) {
        Some("youtube") => {
            only_keys(t, &["kind", "id", "title"])
                && title_ok
                && t.get("id").and_then(Value::as_str).is_some_and(|id| {
                    id.len() == 11
                        && id
                            .bytes()
                            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                })
        }
        Some("video") => {
            only_keys(t, &["kind", "url", "poster", "title"])
                && title_ok
                && t.get("url").is_some_and(|url| {
                    https(url)
                        && url.as_str().is_some_and(|u| {
                            let path = u
                                .split(['?', '#'])
                                .next()
                                .unwrap_or("")
                                .to_ascii_lowercase();
                            path.ends_with(".mp4") || path.ends_with(".webm")
                        })
                })
                && t.get("poster").is_some_and(media)
        }
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let path = format!(
            "{}/../../index-template/test/fixtures/store/{name}",
            env!("CARGO_MANIFEST_DIR")
        );
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap()
    }

    #[test]
    fn shared_fixtures_agree_with_the_node_validator() {
        let dir = format!(
            "{}/../../index-template/test/fixtures/store",
            env!("CARGO_MANIFEST_DIR")
        );
        let mut seen = 0;
        for entry in std::fs::read_dir(dir).unwrap() {
            let name = entry.unwrap().file_name().into_string().unwrap();
            let errors = validate(&fixture(&name));
            if name.starts_with("valid-") {
                assert!(errors.is_empty(), "{name}: {errors:?}");
            } else {
                assert!(!errors.is_empty(), "{name} should be rejected");
            }
            seen += 1;
        }
        assert!(seen >= 4);
    }

    #[test]
    fn full_listing_is_valid_and_each_rule_reports_its_field() {
        let full = fixture("valid-full.json");
        assert!(validate(&full).is_empty());
        let broken = |field: &str, value: Value| {
            let mut store = full.clone();
            store[field] = value;
            validate(&store).join("; ")
        };
        assert!(broken("release", serde_json::json!({"status": "soon"})).contains("release"));
        assert!(broken(
            "languages",
            serde_json::json!({"english": {"interface": true}})
        )
        .contains("languages"));
        assert!(
            broken("languages", serde_json::json!({"en": {"interface": false}}))
                .contains("languages")
        );
        assert!(
            broken("features", serde_json::json!(["controller", "controller"]))
                .contains("features")
        );
        assert!(broken(
            "trailers",
            serde_json::json!([{"kind": "youtube", "id": "short"}])
        )
        .contains("trailers[0]"));
        assert!(broken("trailers", serde_json::json!([{"kind": "video", "url": "https://cdn.example/a.mov", "poster": full["cover"]}])).contains("trailers[0]"));
        assert!(broken(
            "links",
            serde_json::json!({"discord": "http://insecure.example"})
        )
        .contains("links"));
        assert!(broken(
            "links",
            serde_json::json!({"myspace": "https://example.com"})
        )
        .contains("links"));
        assert!(broken(
            "system_requirements",
            serde_json::json!({"windows-x64": {"minimum": {"ram": "8 GB"}}})
        )
        .contains("system_requirements"));
        assert!(broken("max_players", serde_json::json!(0)).contains("max_players"));
        assert!(broken(
            "release",
            serde_json::json!({"status": "released", "date": "2026-13-01"})
        )
        .contains("release"));
        assert!(broken("bogus", serde_json::json!(1)).contains("unknown field bogus"));
    }

    #[test]
    fn store_file_names_and_language_tags() {
        for name in [
            "store.json",
            "description.md",
            "changelog.md",
            "description.zh-Hans.md",
            "changelog.pt-BR.md",
            "description.es-419.md",
        ] {
            assert!(is_store_file_name(name), "{name}");
        }
        for name in [
            "targets.json",
            "readme.md",
            "description.english.md",
            "description..md",
            "description.zh_CN.md",
            "changelog.md.bak",
            "../description.md",
        ] {
            assert!(!is_store_file_name(name), "{name}");
        }
        assert!(is_language_tag("en") && is_language_tag("zh-Hans") && is_language_tag("es-419"));
        assert!(
            !is_language_tag("e")
                && !is_language_tag("english")
                && !is_language_tag("zh-")
                && !is_language_tag("zh_CN")
        );
    }
}
