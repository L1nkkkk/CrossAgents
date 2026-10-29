//! What models cost, for the usage page's dollar figures (docs/DESIGN.md §11, usage series).
//!
//! The vendors' public price lists, in USD per million tokens, matched by model id prefix:
//! the longest row that fits wins, so `claude-sonnet-4-5-20250929` is a Sonnet 4.5 call and
//! `gpt-5-mini` is not a `gpt-5` one. A model with no row is left unpriced (the caller
//! reports it) rather than guessed at; `claude-opus-5-5` (a generation newer than any
//! published price) is priced with its family's newest published row, which is written down
//! per row below.

/// USD per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    /// Input tokens that were not served from the prompt cache.
    pub input: f64,
    /// Input tokens served from the prompt cache.
    pub cached_input: f64,
    /// Output tokens, reasoning included.
    pub output: f64,
}

impl Price {
    /// What these tokens cost, in USD. `new_input` is the input that missed the cache;
    /// cache writes are counted there (they are billed at the input rate here).
    pub fn cost(&self, cached_input: u64, new_input: u64, output: u64) -> f64 {
        (cached_input as f64 * self.cached_input
            + new_input as f64 * self.input
            + output as f64 * self.output)
            / 1_000_000.0
    }
}

/// One row of the table: the model ids it covers, and its prices.
struct Row {
    prefix: &'static str,
    price: Price,
}

const fn row(prefix: &'static str, input: f64, cached_input: f64, output: f64) -> Row {
    Row {
        prefix,
        price: Price {
            input,
            cached_input,
            output,
        },
    }
}

/// Prices per million tokens. Sources, and when they were read:
///
/// * Anthropic, <https://www.anthropic.com/pricing> (read 2025-11-25):
///   Opus 4.5 $5 / $0.50 / $25; Opus 4.1 and Opus 4 $15 / $1.50 / $75; Sonnet 4.5 and 4
///   $3 / $0.30 / $15; Haiku 4.5 $1 / $0.10 / $5; Haiku 3.5 $0.80 / $0.08 / $4; Haiku 3
///   $0.25 / $0.03 / $1.25. A family's bare prefix (`claude-opus`) carries the newest
///   published generation of that family and stands for generations the list does not name.
/// * OpenAI, <https://openai.com/api/pricing> (read 2025-11-25): GPT-5.1 and GPT-5
///   $1.25 / $0.125 / $10; GPT-5 mini $0.25 / $0.025 / $2; GPT-5 nano $0.05 / $0.005 /
///   $0.40; GPT-4.1 $2 / $0.50 / $8, mini $0.40 / $0.10 / $1.60, nano $0.10 / $0.025 /
///   $0.40; GPT-4o $2.50 / $1.25 / $10, mini $0.15 / $0.075 / $0.60; o3 $2 / $0.50 / $8,
///   o3-mini $1.10 / $0.44 / $4.40, o4-mini $1.10 / $0.275 / $4.40; o1 $15 / $7.50 / $60.
///
/// CodeBuddy bills in credits, not dollars, and its models have no USD price here; they are
/// reported as unpriced.
const ROWS: &[Row] = &[
    // Claude
    row("claude-opus-4-5", 5.0, 0.50, 25.0),
    row("claude-opus-4-1", 15.0, 1.50, 75.0),
    row("claude-opus-4", 15.0, 1.50, 75.0),
    row("claude-3-opus", 15.0, 1.50, 75.0),
    row("claude-opus-3", 15.0, 1.50, 75.0),
    row("claude-sonnet-4-5", 3.0, 0.30, 15.0),
    row("claude-sonnet-4", 3.0, 0.30, 15.0),
    row("claude-3-7-sonnet", 3.0, 0.30, 15.0),
    row("claude-3-5-sonnet", 3.0, 0.30, 15.0),
    row("claude-haiku-4-5", 1.0, 0.10, 5.0),
    row("claude-3-5-haiku", 0.80, 0.08, 4.0),
    row("claude-3-haiku", 0.25, 0.03, 1.25),
    // Families: the newest published generation, for ids the list above does not name.
    row("claude-opus", 5.0, 0.50, 25.0),
    row("claude-sonnet", 3.0, 0.30, 15.0),
    row("claude-haiku", 1.0, 0.10, 5.0),
    // OpenAI
    row("gpt-5.1", 1.25, 0.125, 10.0),
    row("gpt-5-mini", 0.25, 0.025, 2.0),
    row("gpt-5-nano", 0.05, 0.005, 0.40),
    row("gpt-5", 1.25, 0.125, 10.0),
    row("gpt-4.1-mini", 0.40, 0.10, 1.60),
    row("gpt-4.1-nano", 0.10, 0.025, 0.40),
    row("gpt-4.1", 2.0, 0.50, 8.0),
    row("gpt-4o-mini", 0.15, 0.075, 0.60),
    row("gpt-4o", 2.50, 1.25, 10.0),
    row("o3-mini", 1.10, 0.44, 4.40),
    row("o4-mini", 1.10, 0.275, 4.40),
    row("o3", 2.0, 0.50, 8.0),
    row("o1", 15.0, 7.50, 60.0),
];

/// The price of `model` (e.g. `claude-sonnet-4-5-20250929`), or `None` when no row covers
/// it. Matching ignores case and a vendor namespace (`anthropic/`, `openai/`); the id has to
/// end or continue with `-` after a row's prefix, so `gpt-5` does not cover `gpt-5.6-…`.
pub fn price(model: &str) -> Option<Price> {
    let id = model.trim().to_lowercase();
    let id = id.rsplit('/').next().unwrap_or(&id);
    ROWS.iter()
        .filter(|row| {
            id.strip_prefix(row.prefix)
                .is_some_and(|rest| rest.is_empty() || rest.starts_with(['-', ':']))
        })
        .max_by_key(|row| row.prefix.len())
        .map(|row| row.price)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_the_longest_row_and_ignores_nothing_it_does_not_know() {
        assert_eq!(
            price("claude-opus-5-5"),
            Some(Price {
                input: 5.0,
                cached_input: 0.50,
                output: 25.0
            })
        );
        assert_eq!(price("claude-opus-4-1-20250805").unwrap().input, 15.0);
        let sonnet = price("claude-sonnet-4-5-20250929").unwrap();
        assert_eq!(
            (sonnet.input, sonnet.cached_input, sonnet.output),
            (3.0, 0.30, 15.0)
        );
        assert_eq!(price("anthropic/claude-haiku-4-5").unwrap().output, 5.0);
        assert_eq!(price("GPT-5-mini").unwrap().input, 0.25);
        // A newer major version of an OpenAI family is not assumed to cost what the last
        // published one does.
        assert_eq!(price("gpt-5.6-luna"), None);
        assert_eq!(price("gpt-6-astra"), None);
        assert_eq!(price("hy4-preview"), None);
        assert_eq!(price("glm-5.3"), None);
    }

    #[test]
    fn cost_is_per_million_tokens() {
        let price = price("claude-sonnet-4-5").unwrap();
        // 1M cached reads, 1M new input tokens, 1M output.
        let cost = price.cost(1_000_000, 1_000_000, 1_000_000);
        assert!((cost - 18.30).abs() < 1e-9, "{cost}");
        assert_eq!(price.cost(0, 0, 0), 0.0);
    }
}
