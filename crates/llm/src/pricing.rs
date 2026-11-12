// What a request cost, worked out from its token counts. Anthropic first-party API
// list prices, taken from https://platform.claude.com/docs/en/about-claude/pricing
// on 2026-11-12. Check that page when a model is added or prices change.
// These are estimates for the user's information, not billing data.

use crate::Usage;

/// US dollars per million tokens.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Price {
    pub input: f64,
    /// 5-minute cache writes, which is what InterCraft's requests use.
    pub cache_write: f64,
    pub cache_read: f64,
    pub output: f64,
}

/// Haiku 5.5 charges more once a prompt passes this many tokens.
const HAIKU_LONG_PROMPT: u64 = 100_000;

/// The list price of an Anthropic model for a request of this size, or None for a
/// model this table doesn't know.
pub fn anthropic_price(model: &str, usage: &Usage) -> Option<Price> {
    let price = |input, cache_write, cache_read, output| Price { input, cache_write, cache_read, output };
    Some(match model {
        "claude-opus-5-5" => price(4.0, 5.0, 0.20, 20.0),
        "claude-sonnet-5-5" => price(2.0, 2.5, 0.10, 10.0),
        "claude-fable-5-2" => price(8.0, 10.0, 0.20, 40.0),
        "claude-haiku-5-5" if prompt_tokens(usage) > HAIKU_LONG_PROMPT => price(0.50, 0.625, 0.05, 2.50),
        "claude-haiku-5-5" => price(0.10, 0.125, 0.01, 0.50),
        _ => return None,
    })
}

/// Everything sent in the request: new input, cache reads and cache writes.
fn prompt_tokens(usage: &Usage) -> u64 {
    usage.input_tokens + usage.cache_read_tokens + usage.cache_write_tokens
}

/// Dollars for one request.
pub fn cost(price: Price, usage: &Usage) -> f64 {
    let per_token = |dollars_per_million: f64, tokens: u64| dollars_per_million * tokens as f64 / 1_000_000.0;
    per_token(price.input, usage.input_tokens)
        + per_token(price.cache_write, usage.cache_write_tokens)
        + per_token(price.cache_read, usage.cache_read_tokens)
        + per_token(price.output, usage.output_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage(input: u64, read: u64, write: u64, output: u64) -> Usage {
        Usage { input_tokens: input, cache_read_tokens: read, cache_write_tokens: write, output_tokens: output }
    }

    #[test]
    fn costs_match_hand_worked_figures() {
        // Opus 5.5: 2,000 new input, 30,000 cached, 1,000 written, 800 out.
        // 0.008 + 0.006 + 0.005 + 0.016 = $0.035
        let u = usage(2_000, 30_000, 1_000, 800);
        let opus = anthropic_price("claude-opus-5-5", &u).unwrap();
        assert!((cost(opus, &u) - 0.035).abs() < 1e-9);

        // Sonnet 5.5, a million tokens of each kind: 2 + 0.1 + 2.5 + 10.
        let m = usage(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        let sonnet = anthropic_price("claude-sonnet-5-5", &m).unwrap();
        assert!((cost(sonnet, &m) - 14.6).abs() < 1e-9);
    }

    #[test]
    fn haiku_costs_more_past_100k_tokens_of_prompt() {
        let short = usage(50_000, 49_000, 0, 1_000);
        let long = usage(50_000, 51_000, 0, 1_000);
        assert_eq!(anthropic_price("claude-haiku-5-5", &short).unwrap().input, 0.10);
        assert_eq!(anthropic_price("claude-haiku-5-5", &long).unwrap().input, 0.50);
    }

    #[test]
    fn unknown_models_have_no_price() {
        assert!(anthropic_price("claude-some-future-model", &Usage::default()).is_none());
    }
}
