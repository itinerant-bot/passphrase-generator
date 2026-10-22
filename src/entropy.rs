use num_bigint::BigUint;

use crate::{Error, Generator, OsRandom, RandomSource, random_index};

const MAX_PLAN_CELLS: usize = 250_000;
const MAX_EFFECTIVE_LENGTH: usize = 4_096;

/// Entropy of the generation distribution, measured in bits.
#[derive(Clone, Copy, Debug)]
pub struct Entropy {
    /// Password entropy, or a conservative password min-entropy lower bound
    /// when an ambiguous separator can merge different word sequences.
    pub bits: f64,
    /// Exact log2 of the number of eligible word sequences (rounded to f64).
    pub word_sequence_bits: f64,
    /// True when `bits` is a lower bound rather than the exact entropy.
    pub is_lower_bound: bool,
}

#[derive(Debug)]
struct WordGroup {
    length: usize,
    start: usize,
    end: usize,
}

/// Prepared counts and sampler for one set of generation constraints.
/// Every eligible sequence has equal probability. With scrambling enabled,
/// sequences contain distinct rendered word variants rather than source words.
pub struct Plan<'a> {
    generator: &'a Generator,
    groups: Vec<WordGroup>,
    // rows[k][b] counts suffixes with <= k words and <= b bytes, charging
    // the separator before EVERY suffix word. Each row includes one stop choice.
    // Rows saturate above the longest possible suffix; accessor clamps budgets.
    rows: Vec<Vec<BigUint>>,
    max_words: usize,
    outcomes: BigUint,
    max_decodings: BigUint,
    entropy: Entropy,
    scramble: Option<crate::scramble::ScramblePlan>,
}

impl<'a> Plan<'a> {
    pub(crate) fn new(generator: &'a Generator) -> Result<Self, Error> {
        if generator.complexity.amount > 100 {
            return Err(Error::InvalidScrambleAmount);
        }
        let max_len = generator.max_len;
        let shortest = generator.words[0].len();
        let longest = generator.words.last().unwrap().len();
        let separator_len = generator.separator.len();
        let physical_cap = match shortest.checked_add(separator_len) {
            Some(cost) => 1 + (max_len - shortest) / cost,
            None => 1,
        };
        let max_words = generator
            .max_words
            .map_or(physical_cap, |limit| limit.get().min(physical_cap));
        let longest_cost = longest.saturating_add(separator_len);
        let effective_length = longest_cost
            .checked_mul(max_words - 1)
            .and_then(|tail| tail.checked_add(longest))
            .unwrap_or(max_len)
            .min(max_len);
        if effective_length > MAX_EFFECTIVE_LENGTH || max_words > MAX_PLAN_CELLS {
            return Err(Error::PlanningLimit);
        }

        let mut groups: Vec<WordGroup> = Vec::new();
        for (index, word) in generator.words.iter().enumerate() {
            if let Some(group) = groups.last_mut().filter(|group| group.length == word.len()) {
                group.end = index + 1;
            } else {
                groups.push(WordGroup {
                    length: word.len(),
                    start: index,
                    end: index + 1,
                });
            }
        }

        let max_tail_budget = max_len - shortest;
        let horizons: Vec<_> = (1..max_words)
            .map(|count| longest_cost.saturating_mul(count).min(max_tail_budget))
            .collect();
        let cells = horizons.iter().try_fold(1usize, |cells, horizon| {
            cells
                .checked_add(horizon + 1)
                .filter(|cells| *cells <= MAX_PLAN_CELLS)
                .ok_or(Error::PlanningLimit)
        })?;
        debug_assert!(cells <= MAX_PLAN_CELLS);
        let mut rows = vec![vec![BigUint::from(1u32)]];
        for horizon in horizons {
            let previous = rows.last().unwrap();
            let mut row = Vec::with_capacity(horizon + 1);
            for budget in 0..=horizon {
                let mut total = BigUint::from(1u32); // Stop this suffix here.
                for group in &groups {
                    let Some(cost) = group.length.checked_add(separator_len) else {
                        break;
                    };
                    if cost > budget {
                        break;
                    }
                    let remaining = (budget - cost).min(previous.len() - 1);
                    total += &previous[remaining] * (group.end - group.start);
                }
                row.push(total);
            }
            rows.push(row);
        }

        let mut plan = Self {
            generator,
            groups,
            rows,
            max_words,
            outcomes: BigUint::default(),
            max_decodings: BigUint::from(1u32),
            scramble: None,
            entropy: Entropy {
                bits: 0.0,
                word_sequence_bits: 0.0,
                is_lower_bound: false,
            },
        };
        // The first word has no separator, and an empty password is excluded.
        for group in &plan.groups {
            if group.length > max_len {
                break;
            }
            plan.outcomes +=
                plan.completions(max_words - 1, max_len - group.length) * (group.end - group.start);
        }
        if !generator.separator.bytes().any(|byte| {
            if generator.complexity.enabled() {
                !(byte.is_ascii_alphabetic()
                    || generator.complexity.digit && byte.is_ascii_digit()
                    || generator.complexity.special
                        && crate::SPECIAL_CHARACTERS.as_bytes().contains(&byte))
            } else {
                !byte.is_ascii_lowercase()
            }
        }) {
            plan.max_decodings = plan.decoding_bound(effective_length);
        }
        let sequence_bits = log2(&plan.outcomes);
        plan.entropy = Entropy {
            bits: (sequence_bits - log2(&plan.max_decodings)).max(0.0),
            word_sequence_bits: sequence_bits,
            is_lower_bound: plan.max_decodings > BigUint::from(1u32),
        };
        if generator.complexity.enabled() {
            let scramble =
                crate::scramble::ScramblePlan::new(generator, max_words, effective_length)?;
            plan.outcomes = scramble.sequence_count().clone();
            plan.entropy.bits = (log2(&plan.outcomes) - log2(&plan.max_decodings)).max(0.0);
            plan.scramble = Some(scramble);
        }
        Ok(plan)
    }

    pub fn entropy(&self) -> Entropy {
        self.entropy
    }

    /// Exact number of nonempty eligible sequences (rendered variants when scrambling).
    pub fn sequence_count(&self) -> &BigUint {
        &self.outcomes
    }

    /// Generate a complete suggestion using fresh secure draws for each word.
    pub fn generate(&self) -> Result<String, Error> {
        self.generate_with(&mut OsRandom)
    }

    fn completions(&self, words: usize, budget: usize) -> &BigUint {
        let row = &self.rows[words];
        &row[budget.min(row.len() - 1)]
    }

    fn cost(&self, group: &WordGroup, first: bool) -> Option<usize> {
        group.length.checked_add(if first {
            0
        } else {
            self.generator.separator.len()
        })
    }

    fn choose_group(
        &self,
        mut ticket: BigUint,
        first: bool,
        words: usize,
        budget: usize,
    ) -> Option<usize> {
        // A concrete word gets probability completions(next) / completions(now):
        // choose its length with mass n * completions(next), then its index
        // uniformly from n words. These factors telescope over the whole
        // sequence, including the final stop of mass 1, to exactly 1 / outcomes.
        if !first {
            if ticket == BigUint::default() {
                return None;
            }
            ticket -= 1u32;
        }
        for (index, group) in self.groups.iter().enumerate() {
            let Some(cost) = self.cost(group, first) else {
                break;
            };
            if cost > budget {
                break;
            }
            let mass = self.completions(words - 1, budget - cost) * (group.end - group.start);
            if ticket < mass {
                return Some(index);
            }
            ticket -= mass;
        }
        unreachable!("ticket must select a stop choice or a fitting word group")
    }

    fn generate_with(&self, random: &mut impl RandomSource) -> Result<String, Error> {
        if let Some(scramble) = &self.scramble {
            return scramble.generate(self.generator, random);
        }
        let mut password = String::new();
        let mut budget = self.generator.max_len;
        let mut words = self.max_words;
        let mut first = true;
        while words > 0 {
            let total = if first {
                &self.outcomes
            } else {
                self.completions(words, budget)
            };
            if !first && *total == BigUint::from(1u32) {
                break;
            }
            let ticket = random_below(random, total)?;
            let Some(group_index) = self.choose_group(ticket, first, words, budget) else {
                break;
            };
            let group = &self.groups[group_index];
            // Conditional on a length, every word has the same number of
            // suffixes. This is a fresh unbiased draw in the word-index range.
            let word_index = group.start + random_index(random, group.end - group.start)?;
            if !first {
                password.push_str(&self.generator.separator);
            }
            password.push_str(self.generator.words[word_index]);
            budget -= self.cost(group, first).unwrap();
            words -= 1;
            first = false;
        }
        Ok(password)
    }

    // With an ambiguous separator, a rendered password may have several
    // preimages. Every word-length pattern decodes a given string at most once.
    // Count patterns by EXACT rendered length, then take the largest multiplicity
    // bound across lengths. Thus P(password) <= max_decodings / outcomes.
    fn decoding_bound(&self, length_limit: usize) -> BigUint {
        let mut previous = vec![BigUint::default(); length_limit + 1];
        previous[0] = BigUint::from(1u32);
        let mut totals = vec![BigUint::default(); length_limit + 1];
        for count in 1..=self.max_words {
            let mut current = vec![BigUint::default(); length_limit + 1];
            for group in &self.groups {
                let Some(cost) = self.cost(group, count == 1) else {
                    break;
                };
                if cost > length_limit {
                    break;
                }
                for length in cost..=length_limit {
                    current[length] += &previous[length - cost];
                }
            }
            for (total, patterns) in totals.iter_mut().zip(&current) {
                *total += patterns;
            }
            previous = current;
        }
        totals.into_iter().max().unwrap()
    }
}

// Counts and selection weights stay exact integers. Only the displayed log2
// uses floating point, from the highest 53 bits to avoid overflowing f64.
fn log2(value: &BigUint) -> f64 {
    let shift = value.bits().saturating_sub(53) as usize;
    let leading = (value >> shift).to_u64_digits()[0];
    (leading as f64).log2() + shift as f64
}

pub(crate) fn random_below(
    random: &mut impl RandomSource,
    bound: &BigUint,
) -> Result<BigUint, Error> {
    assert!(*bound > BigUint::default());
    let bits = (bound - 1u32).bits() as usize;
    if bits == 0 {
        return Ok(BigUint::default());
    }
    let mut bytes = vec![0u8; bits.div_ceil(8)];
    loop {
        for chunk in bytes.chunks_mut(8) {
            let draw = random.next_u64().map_err(Error::Randomness)?.to_le_bytes();
            chunk.copy_from_slice(&draw[..chunk.len()]);
        }
        if !bits.is_multiple_of(8) {
            *bytes.last_mut().unwrap() &= (1u8 << (bits % 8)) - 1;
        }
        let value = BigUint::from_bytes_le(&bytes);
        if &value < bound {
            return Ok(value);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashMap, VecDeque};
    use std::num::NonZeroUsize;

    struct Scripted(VecDeque<Result<u64, getrandom::Error>>);

    impl Scripted {
        fn values(values: &[u64]) -> Self {
            Self(values.iter().copied().map(Ok).collect())
        }
    }

    impl RandomSource for Scripted {
        fn next_u64(&mut self) -> Result<u64, getrandom::Error> {
            self.0.pop_front().expect("test exhausted its random draws")
        }
    }

    fn fixture(max_len: usize, separator: &str, cap: usize, words: Vec<&'static str>) -> Generator {
        Generator {
            words,
            max_len,
            max_words: NonZeroUsize::new(cap),
            separator: separator.to_owned(),
            complexity: crate::Complexity::default(),
        }
    }

    // Independent brute-force oracle: enumerate actual sequences and render
    // each one. This detects counting mistakes and collisions in small spaces.
    fn enumerate(generator: &Generator, prefix: &str, count: usize, outputs: &mut Vec<String>) {
        if count > 0 {
            outputs.push(prefix.to_owned());
        }
        if count == generator.max_words.unwrap().get() {
            return;
        }
        for word in &generator.words {
            let password = if count == 0 {
                word.to_string()
            } else {
                format!("{prefix}{}{word}", generator.separator)
            };
            if password.len() <= generator.max_len {
                enumerate(generator, &password, count + 1, outputs);
            }
        }
    }

    #[test]
    fn exact_counts_match_exhaustive_enumeration_across_both_caps_and_separators() {
        for budget in 4..=18 {
            for cap in 1..=3 {
                for separator in ["", "-", "::", "a", "X"] {
                    let generator = fixture(
                        budget,
                        separator,
                        cap,
                        vec!["aaaa", "bbbb", "aaaaa", "aaaaaaaa"],
                    );
                    let plan = generator.prepare().unwrap();
                    let mut outputs = Vec::new();
                    enumerate(&generator, "", 0, &mut outputs);
                    assert_eq!(
                        *plan.sequence_count(),
                        BigUint::from(outputs.len()),
                        "budget={budget}, cap={cap}, separator={separator:?}"
                    );
                    let mut multiplicities = HashMap::new();
                    for output in &outputs {
                        *multiplicities.entry(output).or_insert(0usize) += 1;
                    }
                    let actual_max = *multiplicities.values().max().unwrap();
                    assert!(plan.max_decodings >= BigUint::from(actual_max));
                    let actual_min_entropy = (outputs.len() as f64 / actual_max as f64).log2();
                    assert!(plan.entropy().bits <= actual_min_entropy + 1e-10);
                    if !plan.entropy().is_lower_bound {
                        assert_eq!(actual_max, 1);
                        assert!(
                            (plan.entropy().bits - (outputs.len() as f64).log2()).abs() < 1e-10
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn choices_are_weighted_by_valid_suffix_counts_instead_of_word_length() {
        let generator = fixture(11, "-", 2, vec!["aaaa", "bbbbbb"]);
        let plan = generator.prepare().unwrap();
        // Five sequences: aaaa; bbbbbb; aaaa-aaaa; aaaa-bbbbbb; bbbbbb-aaaa.
        assert_eq!(*plan.sequence_count(), BigUint::from(5u32));
        for ticket in 0..5u32 {
            assert_eq!(
                plan.choose_group(ticket.into(), true, 2, 11),
                Some(if ticket < 3 { 0 } else { 1 })
            );
        }
        assert_eq!(plan.choose_group(0u32.into(), false, 1, 5), None);
        assert_eq!(plan.choose_group(1u32.into(), false, 1, 5), Some(0));
        let mut random = Scripted::values(&[3, 0, 1, 0]);
        assert_eq!(plan.generate_with(&mut random).unwrap(), "bbbbbb-aaaa");
        assert!(random.0.is_empty());
    }

    #[test]
    fn every_length_group_has_its_own_fresh_secure_word_index_draw() {
        let generator = fixture(9, "-", 2, vec!["aaaa", "bbbb"]);
        let plan = generator.prepare().unwrap();
        assert_eq!(*plan.sequence_count(), BigUint::from(6u32));
        let mut random = Scripted::values(&[0, 1, 2, 0]);
        assert_eq!(plan.generate_with(&mut random).unwrap(), "bbbb-aaaa");
        assert!(random.0.is_empty());
    }

    #[test]
    fn valid_early_stop_is_part_of_the_uniform_space_but_empty_password_is_not() {
        let generator = fixture(9, "-", 2, vec!["aaaa"]);
        let plan = generator.prepare().unwrap();
        assert_eq!(*plan.sequence_count(), BigUint::from(2u32));
        let mut stop = Scripted::values(&[0, 0, 0]);
        assert_eq!(plan.generate_with(&mut stop).unwrap(), "aaaa");
        let mut continue_drawing = Scripted::values(&[0, 0, 1, 0]);
        assert_eq!(
            plan.generate_with(&mut continue_drawing).unwrap(),
            "aaaa-aaaa"
        );
    }

    #[test]
    fn both_caps_are_respected_and_no_extra_draw_is_needed_after_the_word_cap() {
        for (budget, cap) in [(63, 2), (9, 5)] {
            let generator = fixture(budget, "-", cap, vec!["aaaa"]);
            let plan = generator.prepare().unwrap();
            let mut random = Scripted::values(&[0, 0, 1, 0]);
            assert_eq!(plan.generate_with(&mut random).unwrap(), "aaaa-aaaa");
            assert!(random.0.is_empty());
        }
    }

    #[test]
    fn fixed_separator_contributes_zero_bits_and_only_consumes_length() {
        for separator in ["-", " ", "!", "::", "", "abc"] {
            let generator = fixture(63, separator, 1, vec!["aaaa", "bbbb"]);
            let entropy = generator.prepare().unwrap().entropy();
            assert_eq!(entropy.bits, 1.0);
            assert!(!entropy.is_lower_bound);
        }
        let dash = fixture(9, "-", 2, vec!["aaaa", "bbbb"]);
        let space = fixture(9, " ", 2, vec!["aaaa", "bbbb"]);
        let colon = fixture(9, "::", 2, vec!["aaaa", "bbbb"]);
        assert_eq!(
            dash.prepare().unwrap().entropy().bits,
            space.prepare().unwrap().entropy().bits
        );
        assert_eq!(
            *colon.prepare().unwrap().sequence_count(),
            BigUint::from(2u32)
        );
    }

    #[test]
    fn concatenation_collisions_reduce_the_reported_password_entropy() {
        let generator = fixture(8, "", 2, vec!["aaaa", "aaaaaaaa"]);
        let plan = generator.prepare().unwrap();
        // Three sequences render to two strings; "aaaaaaaa" has probability 2/3.
        assert_eq!(*plan.sequence_count(), BigUint::from(3u32));
        assert_eq!(plan.max_decodings, BigUint::from(2u32));
        assert!(plan.entropy().is_lower_bound);
        assert!((plan.entropy().bits - (3f64 / 2.0).log2()).abs() < 1e-10);
    }

    #[test]
    fn large_integer_sampling_masks_high_bits_and_rejects_out_of_range_values() {
        let bound = (BigUint::from(1u32) << 80usize) + 5u32;
        let mut random = Scripted::values(&[u64::MAX, 1 << 16, 4, 1 << 16]);
        assert_eq!(random_below(&mut random, &bound).unwrap(), &bound - 1u32);
        assert!(random.0.is_empty());
        let mut random = Scripted::values(&[7, 6, 5, 4]);
        assert_eq!(
            random_below(&mut random, &BigUint::from(5u32)).unwrap(),
            BigUint::from(4u32)
        );
        assert!(random.0.is_empty());
    }

    #[test]
    fn entropy_calculation_handles_spaces_far_larger_than_u64_or_f64() {
        let generator = fixture(500, "-", 100, vec!["aaaa", "bbbb"]);
        let plan = generator.prepare().unwrap();
        let expected = (BigUint::from(1u32) << 101usize) - 2u32;
        assert_eq!(*plan.sequence_count(), expected);
        assert!(plan.entropy().bits.is_finite());
        assert!((plan.entropy().bits - 101.0).abs() < 1e-10);
        assert_eq!(log2(&(BigUint::from(1u32) << 5000usize)), 5000.0);
    }

    #[test]
    fn removing_a_constraint_or_adding_words_never_reduces_the_exact_space() {
        let base = fixture(13, "-", 2, vec!["aaaa", "bbbbbb"]);
        let longer = fixture(18, "-", 2, vec!["aaaa", "bbbbbb"]);
        let more_words = fixture(13, "-", 3, vec!["aaaa", "bbbbbb"]);
        let larger_dictionary = fixture(13, "-", 2, vec!["aaaa", "bbbb", "bbbbbb"]);
        let base_count = base.prepare().unwrap().sequence_count().clone();
        for generator in [longer, more_words, larger_dictionary] {
            assert!(*generator.prepare().unwrap().sequence_count() >= base_count);
        }
    }

    #[test]
    fn resource_limits_fail_cleanly_but_large_length_with_a_small_word_cap_is_safe() {
        let cells_exceeded = fixture(2_048, "-", 0, vec!["aaaa"]);
        assert!(matches!(
            cells_exceeded.prepare(),
            Err(Error::PlanningLimit)
        ));
        let generator = fixture(usize::MAX, "-", 0, vec!["aaaa"]);
        assert!(matches!(generator.prepare(), Err(Error::PlanningLimit)));
        let generator = generator.with_max_words(NonZeroUsize::new(1).unwrap());
        assert_eq!(
            *generator.prepare().unwrap().sequence_count(),
            BigUint::from(1u32)
        );
    }

    #[test]
    fn rng_failure_aborts_instead_of_returning_a_partial_password() {
        let generator = fixture(9, "-", 2, vec!["aaaa"]);
        let plan = generator.prepare().unwrap();
        for values in [
            vec![Err(getrandom::Error::UNSUPPORTED)],
            vec![Ok(0), Err(getrandom::Error::UNSUPPORTED)],
            vec![Ok(0), Ok(0), Err(getrandom::Error::UNSUPPORTED)],
        ] {
            let mut random = Scripted(values.into());
            assert!(matches!(
                plan.generate_with(&mut random),
                Err(Error::Randomness(_))
            ));
        }
    }
}
