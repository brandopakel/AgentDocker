//! The person's own price table applied to reported tokens: an estimate,
//! never a bill. Nothing here knows a provider's real prices; the table
//! is empty unless `agentd.toml` fills it, and a row no price matches has
//! no cost, not a zero.
//!
//! A price is US dollars per million tokens for uncached input, cache
//! reads, cache writes and output. A reported input total includes its
//! cache reads and writes (see the adapters), so the uncached share is the
//! total less both; reasoning tokens are inside output and are not priced
//! again. Costs are kept in whole microdollars, which is tokens times the
//! price per million: exact enough for any table and comparable like a
//! count.

use super::{Aggregate, Coverage};
use serde::{Deserialize, Serialize};

/// At most this many entries in a table.
pub const PRICES: usize = 256;

/// One entry: which usage it prices and what at.
#[derive(Clone, Debug, PartialEq)]
pub struct Price {
    /// The provider the usage must name, compared without case; `None`
    /// prices any provider, named or not. Local transcripts of some tools
    /// name no provider at all, so a price for them leaves this out.
    pub provider: Option<String>,
    /// The model name, or a pattern over it: `*` stands for any run of
    /// characters and `?` for one, compared without case.
    pub model: String,
    /// US dollars per million tokens, each zero or more.
    pub input: f64,
    pub cache_read: f64,
    pub cache_write: f64,
    pub output: f64,
}

/// The prices in the order given; the first that matches is the one.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Table {
    prices: Vec<Price>,
}

impl Table {
    /// A table from entries already checked (`UsageConfig::price_table`
    /// checks a file's).
    pub fn new(prices: Vec<Price>) -> Self {
        Self { prices }
    }

    pub fn is_empty(&self) -> bool {
        self.prices.is_empty()
    }

    /// The first price for usage a provider and model reported; nothing
    /// for usage without a model name, however the table reads.
    pub fn find(&self, provider: Option<&str>, model: Option<&str>) -> Option<&Price> {
        let model = model?;
        self.prices.iter().find(|price| {
            price.provider.as_deref().is_none_or(|wanted| {
                provider.is_some_and(|named| named.eq_ignore_ascii_case(wanted))
            }) && glob(&price.model, model)
        })
    }
}

/// `*` any run, `?` one character, the rest itself, without case.
pub fn glob(pattern: &str, text: &str) -> bool {
    let pattern: Vec<char> = pattern.chars().flat_map(char::to_lowercase).collect();
    let text: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    // Iterative with one backtrack point: `*` matches greedily, and a
    // later mismatch gives one more character to the last star.
    let (mut p, mut t) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while t < text.len() {
        match pattern.get(p) {
            Some('*') => {
                star = Some((p, t));
                p += 1;
            }
            Some(&c) if c == '?' || c == text[t] => {
                p += 1;
                t += 1;
            }
            _ => match star {
                Some((sp, st)) => {
                    p = sp + 1;
                    t = st + 1;
                    star = Some((sp, st + 1));
                }
                None => return false,
            },
        }
    }
    pattern[p..].iter().all(|&c| c == '*')
}

/// A row's estimated cost: the sum over its priced buckets, in whole
/// microdollars, and how much of the row's usage it covers. Coverage
/// follows the token counts': complete only when every priced counter was
/// reported by every sample and collection is caught up; partial when some
/// samples did not say, or some of the row's usage had no price; unknown
/// when no priced counter was reported at all.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReport {
    /// Whole microdollars; `None` when no priced counter was reported.
    pub micro_usd: Option<u64>,
    pub coverage: Coverage,
}

impl CostReport {
    /// `$1.23`, marked `~` when partial, `—` when unknown, as a count is.
    pub fn label(&self) -> String {
        match (self.micro_usd, self.coverage) {
            (None, _) | (_, Coverage::Unknown) => "—".to_owned(),
            (Some(micro), Coverage::Complete) => dollars(micro),
            (Some(micro), Coverage::Partial) => format!("{}~", dollars(micro)),
        }
    }
}

/// Microdollars as `$12.34`; below a cent, `<$0.01` rather than a round
/// zero; `$0.00` only for nothing at all.
pub fn dollars(micro_usd: u64) -> String {
    if micro_usd == 0 {
        return "$0.00".to_owned();
    }
    let cents = micro_usd / 10_000;
    if cents == 0 {
        return "<$0.01".to_owned();
    }
    format!("${}.{:02}", super::thousands(cents / 100), cents % 100)
}

/// The fold of one row's buckets into its cost. Each bucket is added with
/// the price that matched it, or none; the report is read once the
/// row's collection coverage is known.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Costing {
    micro_usd: u64,
    priced: u64,
    unpriced: u64,
    said: bool,
    whole: bool,
}

impl Default for Costing {
    fn default() -> Self {
        Self {
            micro_usd: 0,
            priced: 0,
            unpriced: 0,
            said: false,
            whole: true,
        }
    }
}

impl Costing {
    /// Add one bucket. Overflow refuses the whole fold and changes nothing.
    pub fn add(&mut self, price: Option<&Price>, aggregate: &Aggregate) -> Result<(), &'static str> {
        let Some(price) = price else {
            self.unpriced = self
                .unpriced
                .checked_add(1)
                .ok_or("usage cost arithmetic is out of range")?;
            return Ok(());
        };
        let [input, cache_read, cache_write, output, _reasoning] = aggregate.totals;
        let read_known = cache_read.known_samples > 0;
        let write_known = cache_write.known_samples > 0;
        // The uncached share of an inclusive input total: what the row
        // did not say about caching stays in the input count, priced as
        // input, with the coverage below saying so.
        let uncached = input
            .sum
            .saturating_sub(if read_known { cache_read.sum } else { 0 })
            .saturating_sub(if write_known { cache_write.sum } else { 0 });
        let parts = [
            (uncached, price.input),
            (cache_read.sum, price.cache_read),
            (cache_write.sum, price.cache_write),
            (output.sum, price.output),
        ];
        let mut micro = self.micro_usd;
        for (tokens, per_million) in parts {
            // Microdollars per token is dollars per million tokens.
            let cost = (tokens as f64 * per_million).round();
            if !(0.0..9.0e18).contains(&cost) {
                return Err("usage cost arithmetic is out of range");
            }
            micro = micro
                .checked_add(cost as u64)
                .ok_or("usage cost arithmetic is out of range")?;
        }
        // The counters that bear on the estimate: a counter priced at
        // zero does not, except that cache counters still bear on the
        // uncached share whenever input is priced.
        let bears = [
            price.input > 0.0,
            price.cache_read > 0.0 || price.input > 0.0,
            price.cache_write > 0.0 || price.input > 0.0,
            price.output > 0.0,
        ];
        let relevant = [input, cache_read, cache_write, output]
            .into_iter()
            .zip(bears)
            .filter_map(|(total, bears)| bears.then_some(total));
        let mut said = false;
        let mut whole = true;
        for total in relevant {
            said |= total.known_samples > 0;
            whole &= total.known_samples == aggregate.samples;
        }
        self.micro_usd = micro;
        self.priced = self
            .priced
            .checked_add(1)
            .ok_or("usage cost arithmetic is out of range")?;
        self.said |= said;
        self.whole &= whole;
        Ok(())
    }

    /// The cost, or nothing when no bucket had a price. `complete` is
    /// the same collection proof the token counts take.
    pub fn report(&self, complete: bool) -> Option<CostReport> {
        if self.priced == 0 {
            return None;
        }
        let coverage = if !self.said {
            Coverage::Unknown
        } else if complete && self.whole && self.unpriced == 0 {
            Coverage::Complete
        } else {
            Coverage::Partial
        };
        Some(CostReport {
            micro_usd: self.said.then_some(self.micro_usd),
            coverage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::usage::Counters;

    fn price(provider: Option<&str>, model: &str) -> Price {
        Price {
            provider: provider.map(str::to_owned),
            model: model.to_owned(),
            input: 3.0,
            cache_read: 0.3,
            cache_write: 3.75,
            output: 15.0,
        }
    }

    #[test]
    fn a_pattern_matches_a_model_name_without_case() {
        assert!(glob("fable-*", "Fable-7-mini"));
        assert!(glob("*", "anything"));
        assert!(glob("*-mini", "fable-7-mini"));
        assert!(glob("fable-?", "fable-7"));
        assert!(glob("fable-7", "FABLE-7"));
        assert!(glob("*mini*", "fable-mini-7"));
        assert!(glob("a*b*c", "axxbyyc"));
        assert!(!glob("fable-?", "fable-77"));
        assert!(!glob("fable-7", "fable-7-mini"));
        assert!(!glob("*-mini", "fable-7"));
        assert!(!glob("", "x"));
        assert!(glob("", ""));
        assert!(glob("**", ""));
    }

    /// The first entry that matches is the price; a provider named must
    /// be the one reported, one left out prices any; no model, no price.
    #[test]
    fn the_first_matching_entry_prices_the_usage() {
        let table = Table::new(vec![
            price(Some("acme"), "fable-7-mini"),
            Price {
                input: 1.0,
                ..price(None, "fable-*")
            },
            price(Some("other"), "*"),
        ]);
        assert_eq!(
            table.find(Some("ACME"), Some("fable-7-mini")).unwrap().input,
            3.0
        );
        assert_eq!(
            table.find(Some("acme"), Some("fable-7")).unwrap().input,
            1.0,
            "the second entry takes what the first did not"
        );
        assert_eq!(table.find(None, Some("fable-7")).unwrap().input, 1.0);
        assert!(
            table.find(None, Some("fable-7-mini")).unwrap().input == 1.0,
            "a price naming a provider does not match usage naming none"
        );
        assert!(table.find(Some("other"), Some("anything")).is_some());
        assert!(table.find(Some("acme"), Some("gizmo")).is_none());
        assert!(table.find(Some("acme"), None).is_none());
        assert!(Table::default().is_empty());
        assert!(Table::default().find(Some("acme"), Some("fable-7")).is_none());
    }

    fn bucket(counters: &[Counters]) -> Aggregate {
        counters
            .iter()
            .fold(Aggregate::default(), |a, c| a.add(c).unwrap())
    }

    /// Cost is tokens times the price per million, in microdollars, the
    /// uncached input the total less its cache shares; a complete row is
    /// complete only when collection is.
    #[test]
    fn cost_prices_each_counter_once_and_takes_the_counts_coverage() {
        let whole = bucket(&[Counters::from_values([
            Some(1_000_000),
            Some(400_000),
            Some(100_000),
            Some(10_000),
            Some(2_000),
        ])]);
        let mut costing = Costing::default();
        costing.add(Some(&price(None, "*")), &whole).unwrap();
        // 500k uncached at $3, 400k reads at $0.30, 100k writes at $3.75,
        // 10k output at $15: 1.50 + 0.12 + 0.375 + 0.15 = $2.145.
        assert_eq!(
            costing.report(true),
            Some(CostReport {
                micro_usd: Some(2_145_000),
                coverage: Coverage::Complete
            })
        );
        assert_eq!(costing.report(true).unwrap().label(), "$2.14");
        assert_eq!(
            costing.report(false),
            Some(CostReport {
                micro_usd: Some(2_145_000),
                coverage: Coverage::Partial
            }),
            "collection not caught up: partial, like a count"
        );
        assert_eq!(costing.report(false).unwrap().label(), "$2.14~");
    }

    /// A row none of whose buckets has a price has no cost; one with a
    /// priced and an unpriced bucket has a partial one; a priced bucket
    /// whose samples did not report keeps the cost partial, and one that
    /// reported nothing priced is unknown.
    #[test]
    fn unpriced_and_unreported_usage_is_never_a_zero_cost() {
        let said = bucket(&[Counters::from_values([
            Some(100),
            Some(0),
            Some(0),
            Some(10),
            None,
        ])]);
        let mut none = Costing::default();
        none.add(None, &said).unwrap();
        assert_eq!(none.report(true), None, "no price, no column");

        let mut mixed = Costing::default();
        mixed.add(Some(&price(None, "*")), &said).unwrap();
        mixed.add(None, &said).unwrap();
        let report = mixed.report(true).unwrap();
        assert_eq!(report.coverage, Coverage::Partial, "some usage unpriced");
        assert_eq!(report.micro_usd, Some(100 * 3 + 10 * 15));
        assert_eq!(report.label(), "<$0.01~");

        let partly = bucket(&[
            Counters::from_values([Some(100), Some(0), Some(0), Some(10), None]),
            Counters::from_values([Some(100), None, None, None, None]),
        ]);
        let mut partial = Costing::default();
        partial.add(Some(&price(None, "*")), &partly).unwrap();
        let report = partial.report(true).unwrap();
        assert_eq!(report.coverage, Coverage::Partial);
        assert_eq!(report.micro_usd, Some(200 * 3 + 10 * 15));

        let unsaid = bucket(&[Counters::from_values([None, None, None, None, Some(5)])]);
        let mut unknown = Costing::default();
        unknown.add(Some(&price(None, "*")), &unsaid).unwrap();
        assert_eq!(
            unknown.report(true),
            Some(CostReport {
                micro_usd: None,
                coverage: Coverage::Unknown
            })
        );
        assert_eq!(unknown.report(true).unwrap().label(), "—");
    }

    /// A counter priced at zero does not keep a cost partial, except that
    /// cache counters still bear on the uncached input while input is
    /// priced; and a reported zero prices as a known zero.
    #[test]
    fn zero_prices_do_not_demand_coverage() {
        let no_cache = bucket(&[Counters::from_values([
            Some(1_000),
            None,
            None,
            Some(10),
            None,
        ])]);
        let flat = Price {
            provider: None,
            model: "*".into(),
            input: 0.0,
            cache_read: 0.0,
            cache_write: 0.0,
            output: 2.0,
        };
        let mut output_only = Costing::default();
        output_only.add(Some(&flat), &no_cache).unwrap();
        assert_eq!(
            output_only.report(true),
            Some(CostReport {
                micro_usd: Some(20),
                coverage: Coverage::Complete
            })
        );
        let mut with_input = Costing::default();
        with_input.add(Some(&price(None, "*")), &no_cache).unwrap();
        let report = with_input.report(true).unwrap();
        assert_eq!(report.micro_usd, Some(3_000 + 150));
        assert_eq!(
            report.coverage,
            Coverage::Partial,
            "the cache share of the input is unknown"
        );
        let empty = bucket(&[Counters::from_values([Some(0), Some(0), Some(0), Some(0), None])]);
        let mut nothing = Costing::default();
        nothing.add(Some(&price(None, "*")), &empty).unwrap();
        assert_eq!(nothing.report(true).unwrap().label(), "$0.00");
    }

    #[test]
    fn dollars_read_to_the_cent_with_thousands() {
        assert_eq!(dollars(0), "$0.00");
        assert_eq!(dollars(9_999), "<$0.01");
        assert_eq!(dollars(10_000), "$0.01");
        assert_eq!(dollars(1_234_567), "$1.23");
        assert_eq!(dollars(1_234_567_890), "$1,234.56");
    }

    #[test]
    fn an_overflowing_cost_refuses_the_fold_unchanged() {
        let huge = bucket(&[Counters::from_values([
            Some(u64::MAX),
            Some(0),
            Some(0),
            Some(0),
            None,
        ])]);
        let mut costing = Costing::default();
        costing.add(Some(&price(None, "*")), &huge).unwrap_err();
        assert_eq!(costing, Costing::default());
    }
}
