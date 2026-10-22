//! Names the hub makes up for agents nobody named: short given names, never a vendor's or a
//! model's word (an agent called "Haiku" reads as the model, and its teammates start calling
//! the model by it), and never a name already taken: a clash gets an ordinal ("Nova 2").

/// Given names to draw from; none is a vendor, product or model word.
const POOL: &[&str] = &[
    "Nova", "Iris", "Atlas", "Orion", "Vega", "Lyra", "Mira", "Sage", "Juno", "Wren", "Ember",
    "Rowan", "Quinn", "Remy", "Kai", "Theo", "Ivy", "Milo", "Zara", "Nico", "Cleo", "Felix",
    "Hazel", "Jasper", "Leo", "Maya", "Noor", "Otto", "Pia", "Ravi", "Suki", "Tess", "Yara",
    "Zeno",
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pool_has_no_vendor_words() {
        for name in POOL {
            assert!(!is_vendor_word(name, &[]), "{name}");
        }
    }

    #[test]
    fn model_words_are_denied() {
        let words = model_words(["gpt-6-luna", "claude-haiku-5-5", "mock-nova"]);
        assert_eq!(words, ["claude", "gpt", "haiku", "luna", "mock", "nova"]);
        assert!(is_vendor_word("Haiku", &[]));
        assert!(is_vendor_word("Luna 2", &[]));
        assert!(is_vendor_word("Haiku金价查询", &[]));
        assert!(!is_vendor_word("金价查询", &[]));
        assert!(is_vendor_word("Nova", &words));
        assert!(!is_vendor_word("Iris", &words));
        // A generated name skips the denied word.
        let names = vec![];
        for seed in 0..POOL.len() {
            assert_ne!(generate(&names, &words, seed), "Nova");
        }
    }

    #[test]
    fn clashes_get_an_ordinal() {
        let names = vec!["Nova".to_owned(), "nova 2".to_owned()];
        assert_eq!(with_ordinal("Nova", &names), "Nova 3");
        assert_eq!(with_ordinal("Iris", &names), "Iris");
        // Generated names skip taken ones, and wrap to ordinals once the pool is used up.
        let first = generate(&names, &[], 0);
        assert!(!taken(&first, &names));
        let all: Vec<String> = POOL.iter().map(|s| s.to_string()).collect();
        assert_eq!(generate(&all, &[], 1), format!("{} 2", POOL[1]));
    }
}
