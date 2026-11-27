//! Names the hub makes up for agents nobody named: Western mythology, astronomy and
//! esoteric/hermetic names scoped by project code and monotonic sequence (`CA-Nyx-1`),
//! never a vendor's or a model's word (an agent called "Haiku" reads as the model, and its
//! teammates start calling the model by it).

use std::collections::HashSet;
use std::path::Path;

/// Curated Western mythology, astronomy and esoteric/hermetic names.
/// None is a vendor, product or known model word; all are clean single ASCII tokens.
const POOL: &[&str] = &[
    // Greek, Roman, Norse, Egyptian & Celtic mythology
    "Nyx",
    "Athena",
    "Apollo",
    "Artemis",
    "Hermes",
    "Helios",
    "Selene",
    "Aether",
    "Erebus",
    "Hyperion",
    "Phoebe",
    "Rhea",
    "Themis",
    "Metis",
    "Iris",
    "Eos",
    "Morpheus",
    "Orpheus",
    "Ariadne",
    "Cassandra",
    "Thalia",
    "Clio",
    "Calliope",
    "Euterpe",
    "Erato",
    "Urania",
    "Hestia",
    "Demeter",
    "Hecate",
    "Nemesis",
    "Nike",
    "Tyche",
    "Freya",
    "Odin",
    "Baldr",
    "Frigg",
    "Tyr",
    "Heimdall",
    "Idunn",
    "Skadi",
    "Mimir",
    "Bragi",
    "Eir",
    "Sigyn",
    "Norn",
    "Thoth",
    "Anubis",
    "Isis",
    "Osiris",
    "Horus",
    "Bastet",
    "Maat",
    "Ptah",
    "Nephthys",
    "Khepri",
    "Brigid",
    "Lugh",
    "Morrigan",
    "Taliesin",
    "Arawn",
    "Janus",
    "Vesta",
    "Minerva",
    "Ceres",
    "Aurora",
    "Zephyr",
    "Boreas",
    "Eurus",
    "Notus",
    "Atlas",
    "Orion",
    "Juno",
    "Daphne",
    "Lycus",
    "Cadmus",
    "Theseus",
    "Electra",
    "Calypso",
    "Circe",
    "Siren",
    // Astronomy: bright stars, constellations, moons & celestial phenomena
    "Sirius",
    "Rigel",
    "Altair",
    "Antares",
    "Aldebaran",
    "Arcturus",
    "Capella",
    "Canopus",
    "Procyon",
    "Spica",
    "Pollux",
    "Castor",
    "Deneb",
    "Fomalhaut",
    "Bellatrix",
    "Mirach",
    "Alcor",
    "Mizar",
    "Regulus",
    "Vega",
    "Lyra",
    "Cygnus",
    "Aquila",
    "Andromeda",
    "Perseus",
    "Draco",
    "Phoenix",
    "Pegasus",
    "Carina",
    "Vela",
    "Cetus",
    "Hydra",
    "Cassiopeia",
    "Cepheus",
    "Auriga",
    "Bootes",
    "Centaurus",
    "Corvus",
    "Delphinus",
    "Eridanus",
    "Fornax",
    "Lacerta",
    "Monoceros",
    "Ophiuchus",
    "Pyxis",
    "Serpens",
    "Triangulum",
    "Solstice",
    "Equinox",
    "Eclipse",
    "Zenith",
    "Nadir",
    "Aphelion",
    "Perihelion",
    "Corona",
    "Nebula",
    "Pulsar",
    "Quasar",
    "Halley",
    "Pallas",
    "Hygieia",
    "Titan",
    "Europa",
    "Enceladus",
    "Oberon",
    "Titania",
    "Triton",
    "Charon",
    "Callisto",
    "Ganymede",
    "Phobos",
    "Deimos",
    "Iapetus",
    "Dione",
    "Tethys",
    "Mimas",
    "Ariel",
    "Umbriel",
    "Miranda",
    "Proteus",
    "Nereid",
    // Esoteric, Hermetic, Alchemy & Tarot
    "Arcana",
    "Azoth",
    "Athanor",
    "Alkahest",
    "Ouroboros",
    "Caduceus",
    "Chalice",
    "Pentacle",
    "Tarot",
    "Hierophant",
    "Magus",
    "Oracle",
    "Sibyl",
    "Augur",
    "Adept",
    "Grimoire",
    "Sigil",
    "Rune",
    "Astral",
    "Umbra",
    "Penumbra",
    "Aegis",
    "Labyrinth",
    "Monolith",
    "Obelisk",
    "Talisman",
    "Cipher",
    "Vesper",
    "Nocturne",
    "Mercurius",
    "Cinnabar",
    "Akasha",
    "Gnosis",
    "Logos",
    "Sophia",
    "Pleroma",
    "Chronos",
    "Kairos",
    "Kether",
    "Binah",
    "Chokmah",
    "Chesed",
    "Tiphereth",
    "Malkuth",
    "Alembic",
    "Crucible",
    "Elixir",
    "Hesperus",
    "Phosphor",
    "Elysium",
    "Styx",
    "Lethe",
    "Avalon",
    "Valhalla",
    "Asgard",
    "Bifrost",
    "Yggdrasil",
    "Sanctum",
    "Arcanum",
    "Aurelia",
    "Caelum",
    "Nox",
];

/// Words that name a vendor, product or model family. The hub adds the words of the models
/// its plugins report (see [`model_words`]), so new models are covered too.
pub const VENDOR_TERMS: &[&str] = &[
    "anthropic",
    "claude",
    "opus",
    "sonnet",
    "haiku",
    "fable",
    "openai",
    "gpt",
    "chatgpt",
    "codex",
    "luna",
    "o1",
    "o3",
    "o4",
    "mini",
    "nano",
    "turbo",
    "google",
    "gemini",
    "gemma",
    "meta",
    "llama",
    "mistral",
    "mixtral",
    "xai",
    "grok",
    "qwen",
    "deepseek",
    "kimi",
    "moonshot",
    "glm",
    "zhipu",
    "doubao",
    "ernie",
    "spark",
    "phi",
    "copilot",
    "cursor",
    "ada",
    "babbage",
    "curie",
    "davinci",
];

/// The words of model ids, e.g. "gpt-6-luna" → ["gpt", "luna"] (numbers dropped).
pub fn model_words<'a>(models: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut words: Vec<String> = models
        .into_iter()
        .flat_map(|m| m.split(|c: char| !c.is_alphanumeric()))
        .filter(|w| w.len() > 1 && !w.chars().all(|c| c.is_ascii_digit()))
        .map(str::to_lowercase)
        .collect();
    words.sort();
    words.dedup();
    words
}

/// Whether `name` contains a vendor term or one of `extra`, as a word, ignoring case.
pub fn is_vendor_word(name: &str, extra: &[String]) -> bool {
    // Words are runs of ASCII letters and digits (every vendor and model word is ASCII), so
    // "Haiku金价" still holds "haiku".
    name.split(|c: char| !c.is_ascii_alphanumeric())
        .map(str::to_lowercase)
        .any(|w| VENDOR_TERMS.contains(&w.as_str()) || extra.contains(&w))
}

fn taken(name: &str, names: &[String]) -> bool {
    names
        .iter()
        .any(|n| n.to_lowercase() == name.to_lowercase())
}

/// `base`, or `base 2`, `base 3`, … whichever is free (names compare ignoring case).
pub fn with_ordinal(base: &str, names: &[String]) -> String {
    if !taken(base, names) {
        return base.to_owned();
    }
    (2..)
        .map(|n| format!("{base} {n}"))
        .find(|candidate| !taken(candidate, names))
        .expect("some ordinal is free")
}

/// A free name from the pool, starting at `seed` so new agents do not all line up in pool
/// order; once the pool is used up, its names come back with ordinals.
pub fn generate(names: &[String], extra_deny: &[String], seed: usize) -> String {
    let pool: Vec<&str> = POOL
        .iter()
        .copied()
        .filter(|n| !is_vendor_word(n, extra_deny))
        .collect();
    let start = seed % pool.len();
    let rotated = pool[start..].iter().chain(&pool[..start]);
    if let Some(free) = rotated.clone().find(|n| !taken(n, names)) {
        return (*free).to_owned();
    }
    with_ordinal(pool[start], names)
}

/// Derives a crisp, deterministic short project code from a workspace directory:
/// - `CrossAgents` → `"CA"` (CamelCase capitals)
/// - `Neuro-Saki` → `"NS"` (delimiter-separated initials)
/// - `Sever` → `"SV"` (initial + first consonant, fallback second letter)
/// - `api` → `"API"` (2–3 char ASCII words stay whole)
/// - standalone chats (under `chats_dir` or empty) → `"CH"`
pub fn project_code(path: Option<&Path>, chats_dir: Option<&Path>) -> String {
    let Some(path) = path else {
        return "CH".to_owned();
    };
    if path.as_os_str().is_empty() {
        return "CH".to_owned();
    }
    if let Some(chats) = chats_dir
        && !chats.as_os_str().is_empty()
        && path.starts_with(chats)
    {
        return "CH".to_owned();
    }
    // Also recognise remote/other-platform standalone chat dirs like `~/.crossagents/chats/<uuid>`.
    let norm = path.to_string_lossy().replace('\\', "/");
    if norm.contains("/.crossagents/chats/") {
        return "CH".to_owned();
    }
    let Some(base) = path
        .file_name()
        .and_then(|s| s.to_str())
        .map(str::trim)
        .filter(|s| !s.is_empty())
    else {
        return "CH".to_owned();
    };
    derive_code_from_label(base)
}

/// Derives a 2–4 character short code from a project folder name.
pub fn derive_code_from_label(label: &str) -> String {
    let label = label.trim();
    if label.is_empty() {
        return "CH".to_owned();
    }
    let tokens: Vec<&str> = label
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty())
        .collect();
    if tokens.is_empty() {
        return "WS".to_owned();
    }

    // Multiple words (e.g. `Neuro-Saki`, `my_web_app`): take first char of each word (up to 4).
    if tokens.len() >= 2 {
        let mut code = String::new();
        for tok in tokens.iter().take(4) {
            if let Some(ch) = tok.chars().next() {
                for up in ch.to_uppercase() {
                    code.push(up);
                }
            }
        }
        if starts_alphabetic(&code) {
            return code;
        }
    }

    // Single token: check CamelCase capitals (`CrossAgents` -> `CA`, `OpenCode` -> `OC`).
    let word = tokens[0];
    if word.is_ascii() {
        let chars: Vec<char> = word.chars().collect();
        let mut caps = String::new();
        for (idx, &ch) in chars.iter().enumerate() {
            if idx == 0 && ch.is_ascii_alphabetic() {
                caps.push(ch.to_ascii_uppercase());
            } else if idx > 0
                && ch.is_ascii_uppercase()
                && !chars[idx - 1].is_ascii_uppercase()
            {
                caps.push(ch);
                if caps.len() >= 4 {
                    break;
                }
            }
        }
        if caps.len() >= 2 && starts_alphabetic(&caps) {
            return caps;
        }
        // Short ASCII word (2–3 chars like `api`, `gui`, `io`): uppercase whole word.
        let alpha_only: Vec<char> = chars.iter().copied().filter(|c| c.is_ascii_alphanumeric()).collect();
        if (2..=3).contains(&alpha_only.len()) && alpha_only[0].is_ascii_alphabetic() {
            return alpha_only
                .iter()
                .map(|c| c.to_ascii_uppercase())
                .collect();
        }
        // Single longer ASCII word (e.g. `Sever` -> `SV`, `Notes` -> `NT`, `Audio` -> `AU`).
        if let Some(&first) = alpha_only.first().filter(|c| c.is_ascii_alphabetic()) {
            let mut out = String::new();
            out.push(first.to_ascii_uppercase());
            let next = alpha_only[1..]
                .iter()
                .copied()
                .find(|c| c.is_ascii_alphabetic() && !is_ascii_vowel(*c))
                .or_else(|| alpha_only.get(1).copied());
            if let Some(ch) = next {
                out.push(ch.to_ascii_uppercase());
            }
            if starts_alphabetic(&out) {
                return out;
            }
        }
        return "WS".to_owned();
    }

    // Non-ASCII (e.g. CJK folder name `笔记项目` -> `笔记`).
    let non_ascii: String = word
        .chars()
        .filter(|c| c.is_alphanumeric())
        .take(2)
        .collect();
    if starts_alphabetic(&non_ascii) {
        non_ascii
    } else {
        "WS".to_owned()
    }
}

fn is_ascii_vowel(c: char) -> bool {
    matches!(c.to_ascii_lowercase(), 'a' | 'e' | 'i' | 'o' | 'u')
}

fn starts_alphabetic(s: &str) -> bool {
    s.chars().next().is_some_and(char::is_alphabetic)
}

/// Extracts `(prefix_lower, middle_word_lower, seq)` from a scoped name like `CA-Nyx-12`.
fn parse_scoped_parts(name: &str) -> Option<(String, String, u32)> {
    let mut parts = name.split('-');
    let prefix = parts.next()?;
    let word = parts.next()?;
    let seq_str = parts.next()?;
    if parts.next().is_some() || prefix.is_empty() || word.is_empty() {
        return None;
    }
    let seq: u32 = seq_str.parse().ok()?;
    Some((prefix.to_lowercase(), word.to_lowercase(), seq))
}

/// Generates a scoped name `<PREFIX>-<Myth/Astro/EsotericWord>-<SEQ>`, e.g. `CA-Nyx-1`.
///
/// - `<SEQ>` is the next monotonic integer for `<PREFIX>` (1, 2, 3, …), so sequence numbers
///   reflect creation order within the project rather than clone collisions.
/// - `<Word>` prefers words never used globally, then words never used in `<PREFIX>`, and
///   never picks a vendor or model word.
pub fn generate_scoped(
    prefix: &str,
    existing_names: &[String],
    extra_deny: &[String],
    seed: usize,
) -> String {
    let clean_prefix = {
        let trimmed = prefix.trim();
        if trimmed.is_empty() || !starts_alphabetic(trimmed) {
            "CH"
        } else {
            trimmed
        }
    };
    let prefix_lower = clean_prefix.to_lowercase();

    let mut max_seq: u32 = 0;
    let mut used_in_prefix: HashSet<String> = HashSet::new();
    let mut used_globally: HashSet<String> = HashSet::new();

    for existing in existing_names {
        if let Some((p, w, seq)) = parse_scoped_parts(existing) {
            used_globally.insert(w.clone());
            if p == prefix_lower {
                max_seq = max_seq.max(seq);
                used_in_prefix.insert(w);
            }
        } else {
            // Also remember standalone legacy names (e.g. "Orion", "Vega 3") as globally used.
            if let Some(first) = existing.split_whitespace().next() {
                used_globally.insert(first.to_lowercase());
            }
        }
    }

    let pool: Vec<&str> = POOL
        .iter()
        .copied()
        .filter(|n| !is_vendor_word(n, extra_deny))
        .collect();
    let start = if pool.is_empty() { 0 } else { seed % pool.len() };
    let rotated = pool[start..].iter().chain(&pool[..start]);

    let word = rotated
        .clone()
        .find(|w| !used_globally.contains(&w.to_lowercase()))
        .or_else(|| {
            rotated
                .clone()
                .find(|w| !used_in_prefix.contains(&w.to_lowercase()))
        })
        .copied()
        .unwrap_or_else(|| pool.get(start).copied().unwrap_or("Nyx"));

    let mut seq = max_seq.saturating_add(1).max(1);
    loop {
        let candidate = format!("{clean_prefix}-{word}-{seq}");
        if !taken(&candidate, existing_names) {
            return candidate;
        }
        seq = seq.saturating_add(1);
    }
}

/// Conversational filler prefixes stripped when distilling a semantic short title.
const FILLER_PREFIXES: &[&str] = &[
    "你看看你能不能",
    "你看看能不能",
    "你帮我看看",
    "帮我看看",
    "帮我查一下",
    "帮我一下",
    "麻烦帮我",
    "麻烦你",
    "请问一下",
    "请问",
    "请帮我",
    "请你",
    "你看看",
    "能不能",
    "可以帮我",
    "帮我",
    "我现在想要一个",
    "我现在想要",
    "我想要一个",
    "我想要",
    "我想",
    "你好啊",
    "你好呀",
    "你好",
    "您好",
    "哈喽",
    "在吗",
    "在不在",
    "好的",
    "好吧",
    "okay",
    "ok",
    "hello",
    "hey there",
    "hey",
    "hi there",
    "hi",
    "please help me",
    "can you help me",
    "could you help me",
    "could you please",
    "can you please",
    "would you please",
    "help me",
    "please",
    "can you",
    "could you",
];

/// Bare greetings or acknowledgements that should not become a session's permanent topic.
const BARE_GREETINGS: &[&str] = &[
    "你好",
    "你好啊",
    "你好呀",
    "您好",
    "哈喽",
    "嗨",
    "在吗",
    "在不在",
    "测试",
    "测一下",
    "ok",
    "okay",
    "hi",
    "hello",
    "hey",
    "test",
    "ping",
    "yo",
    "好",
    "好的",
    "行",
    "嗯",
];

fn trim_lead_punct(s: &str) -> &str {
    s.trim_start_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | '，'
                    | '.'
                    | '。'
                    | '!'
                    | '！'
                    | '?'
                    | '？'
                    | ':'
                    | '：'
                    | ';'
                    | '；'
                    | '、'
                    | '-'
                    | '—'
                    | '>'
                    | '#'
            )
    })
}

fn trim_edge_punct(s: &str) -> &str {
    s.trim_matches(|c: char| {
        c.is_whitespace()
            || matches!(
                c,
                ',' | '，'
                    | '.'
                    | '。'
                    | '!'
                    | '！'
                    | '?'
                    | '？'
                    | ':'
                    | '：'
                    | ';'
                    | '；'
                    | '、'
            )
    })
}

/// Returns `true` when `text` is only a bare greeting / acknowledgement with no real task content.
pub fn is_bare_greeting(text: &str) -> bool {
    let cleaned = trim_edge_punct(text).to_lowercase();
    if cleaned.is_empty() {
        return true;
    }
    BARE_GREETINGS.contains(&cleaned.as_str())
}

/// Extracts a concise semantic short title from a user's message, stripping markdown noise and
/// conversational fillers. Returns `None` for pure greetings (`"你好"`, `"hello"`, `"ok"`) so
/// the session waits for a real task message before locking in a title.
pub fn semantic_topic(text: &str) -> Option<String> {
    const MAX_CHARS: usize = 36;
    let first_line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty() && !line.starts_with("```"))?;
    let mut s = first_line.trim_start_matches(['#', '>', '-', '*', ' ']).trim();
    if is_bare_greeting(s) {
        return None;
    }
    // Iteratively strip polite filler prefixes, while never erasing the whole message.
    for _ in 0..3 {
        let lower = s.to_lowercase();
        let mut matched = false;
        for &prefix in FILLER_PREFIXES {
            if let Some(rest_lower) = lower.strip_prefix(prefix) {
                // For ASCII prefixes, ensure word boundary so "hi" doesn't strip "history".
                if prefix.is_ascii()
                    && rest_lower
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_ascii_alphanumeric())
                {
                    continue;
                }
                // Map byte length carefully: ASCII prefixes have identical byte lengths in
                // lowercase, and CJK prefixes have identical byte lengths in UTF-8.
                if let Some(after) = s.get(prefix.len()..) {
                    let candidate = trim_lead_punct(after).trim();
                    if candidate.chars().count() >= 2 && !is_bare_greeting(candidate) {
                        s = candidate;
                        matched = true;
                        break;
                    }
                }
            }
        }
        if !matched {
            break;
        }
    }
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let trimmed = collapsed.trim();
    if trimmed.is_empty() || is_bare_greeting(trimmed) {
        return None;
    }
    let count = trimmed.chars().count();
    if count <= MAX_CHARS {
        Some(trimmed.to_owned())
    } else {
        let cut: String = trimmed.chars().take(MAX_CHARS - 1).collect();
        Some(format!("{}…", cut.trim_end()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_pool_has_no_vendor_words_or_duplicates() {
        let mut seen = HashSet::new();
        for &name in POOL {
            assert!(!is_vendor_word(name, &[]), "vendor word in POOL: {name}");
            assert!(
                seen.insert(name.to_lowercase()),
                "duplicate name in POOL: {name}"
            );
            assert!(
                crate::validate_agent_name(name).is_ok(),
                "invalid agent name in POOL: {name}"
            );
        }
        assert!(POOL.len() >= 160, "expected a large curated pool, got {}", POOL.len());
    }

    #[test]
    fn model_words_are_denied() {
        let words = model_words(["gpt-6-luna", "claude-haiku-5-5", "mock-nyx"]);
        assert_eq!(words, ["claude", "gpt", "haiku", "luna", "mock", "nyx"]);
        assert!(is_vendor_word("Haiku", &[]));
        assert!(is_vendor_word("Luna 2", &[]));
        assert!(is_vendor_word("Haiku金价查询", &[]));
        assert!(!is_vendor_word("金价查询", &[]));
        assert!(is_vendor_word("Nyx", &words));
        assert!(!is_vendor_word("Iris", &words));
        // A generated name skips the denied word.
        let names = vec![];
        for seed in 0..POOL.len() {
            assert_ne!(generate(&names, &words, seed), "Nyx");
        }
    }

    #[test]
    fn clashes_get_an_ordinal() {
        let names = vec!["Nyx".to_owned(), "nyx 2".to_owned()];
        assert_eq!(with_ordinal("Nyx", &names), "Nyx 3");
        assert_eq!(with_ordinal("Iris", &names), "Iris");
        // Generated names skip taken ones, and wrap to ordinals once the pool is used up.
        let first = generate(&names, &[], 0);
        assert!(!taken(&first, &names));
        let all: Vec<String> = POOL.iter().map(|s| s.to_string()).collect();
        assert_eq!(generate(&all, &[], 1), format!("{} 2", POOL[1]));
    }

    #[test]
    fn project_codes_are_concise_and_deterministic() {
        let chats = PathBuf::from("/home/user/.crossagents/chats");
        assert_eq!(project_code(None, Some(&chats)), "CH");
        assert_eq!(
            project_code(Some(Path::new("/home/user/.crossagents/chats/abc")), Some(&chats)),
            "CH"
        );
        assert_eq!(
            project_code(Some(Path::new("C:/Users/Link/Dev/CrossAgents")), Some(&chats)),
            "CA"
        );
        assert_eq!(
            project_code(Some(Path::new("C:/Users/Link/Dev/Neuro-Saki")), Some(&chats)),
            "NS"
        );
        assert_eq!(
            project_code(Some(Path::new("C:/Users/Link/Dev/Sever")), Some(&chats)),
            "SV"
        );
        assert_eq!(
            project_code(Some(Path::new("/tmp/api")), Some(&chats)),
            "API"
        );
        assert_eq!(
            project_code(Some(Path::new("/tmp/笔记项目")), Some(&chats)),
            "笔记"
        );
    }

    #[test]
    fn scoped_names_follow_project_word_seq_without_number_explosion() {
        let mut names = Vec::new();
        let first = generate_scoped("CA", &names, &[], 0);
        assert_eq!(first, "CA-Nyx-1");
        assert!(crate::validate_agent_name(&first).is_ok());
        names.push(first);

        let second = generate_scoped("CA", &names, &[], 0);
        assert!(second.starts_with("CA-") && second.ends_with("-2"), "{second}");
        assert_ne!(second, "CA-Nyx-2");
        names.push(second);

        // Another project starts its own sequence at 1 and still picks a fresh word.
        let other = generate_scoped("NS", &names, &[], 0);
        assert!(other.starts_with("NS-") && other.ends_with("-1"), "{other}");
        assert!(!other.contains("-Nyx-"));
    }

    #[test]
    fn semantic_topic_skips_greetings_and_strips_filler() {
        assert_eq!(semantic_topic("你好"), None);
        assert_eq!(semantic_topic("hello!"), None);
        assert_eq!(semantic_topic("ok"), None);
        assert_eq!(
            semantic_topic("你看看你能不能解决一下Pia 5会话遇到的问题"),
            Some("解决一下Pia 5会话遇到的问题".to_owned())
        );
        assert_eq!(
            semantic_topic("修一下登录页\n细节如下"),
            Some("修一下登录页".to_owned())
        );
        assert_eq!(
            semantic_topic("please fix the proxy timeout"),
            Some("fix the proxy timeout".to_owned())
        );
        // Word boundary check: "history" must not lose "hi"
        assert_eq!(
            semantic_topic("history view layout"),
            Some("history view layout".to_owned())
        );
    }
}
