//! Count distinct readable word renderings before sampling. A deterministic
//! automaton merges overlapping substitutions from different dictionary words;
//! the outer planner then samples valid rendered sequences uniformly.
use std::collections::{BTreeMap, HashMap};

use num_bigint::BigUint;

use crate::entropy::random_below;
use crate::{Complexity, Error, Generator, RandomSource};

const LOWER: usize = 1;
const UPPER: usize = 2;
const DIGIT: usize = 4;
const SPECIAL: usize = 8;
const MASKS: usize = 16;
const MAX_STATES: usize = 500_000;
const MAX_CELLS: usize = 8_000_000;

type Counts = [BigUint; MASKS];

fn zero_counts() -> Counts {
    std::array::from_fn(|_| BigUint::default())
}

fn class(byte: u8) -> usize {
    match byte {
        b'a'..=b'z' => LOWER,
        b'A'..=b'Z' => UPPER,
        b'0'..=b'9' => DIGIT,
        _ => SPECIAL,
    }
}

/// Letter alternatives deliberately stay small and recognizable. A letter can
/// have several alternatives, and the same alternative can represent more than
/// one letter. The automaton accounts for both without double counting outputs.
fn alternatives(letter: u8, settings: Complexity) -> Vec<u8> {
    let mut choices = vec![letter];
    if settings.uppercase {
        choices.push(letter.to_ascii_uppercase());
    }
    if settings.digit {
        choices.extend_from_slice(match letter {
            b'a' => b"4",
            b'b' => b"8",
            b'e' => b"3",
            b'g' => b"69",
            b'i' | b'l' => b"1",
            b'o' => b"0",
            b'q' => b"9",
            b's' => b"5",
            b't' => b"7",
            b'z' => b"2",
            _ => b"",
        });
    }
    if settings.special {
        choices.extend_from_slice(match letter {
            b'a' => b"@",
            b'e' => b"#@",
            b'g' | b'h' => b"#",
            b'i' => b"!",
            b'l' => b"!_",
            b'o' | b's' => b"%",
            b'u' => b"_",
            _ => b"",
        });
    }
    choices
}

#[derive(Default)]
struct TrieNode {
    children: BTreeMap<u8, usize>,
    terminal: bool,
}

struct Edge {
    byte: u8,
    next: usize,
}

struct State {
    edges: Vec<Edge>,
    /// Number of distinct suffixes by their exact character-class mask.
    counts: Vec<Counts>,
}

struct WordVariants {
    length: usize,
    states: Vec<State>,
    root: usize,
    tracked: bool,
    allowed_counts: Vec<Counts>,
}

struct WordBuilder {
    trie: Vec<TrieNode>,
    states: Vec<State>,
    memo: HashMap<Vec<usize>, usize>,
    settings: Complexity,
    limit: usize,
    tracked: bool,
}

impl WordBuilder {
    fn state(&mut self, nodes: Vec<usize>) -> Result<usize, Error> {
        let key = nodes;
        if let Some(&id) = self.memo.get(&key) {
            return Ok(id);
        }
        if self.states.len() >= MAX_STATES {
            return Err(Error::PlanningLimit);
        }
        let id = self.states.len();
        self.memo.insert(key.clone(), id);
        self.states.push(State {
            edges: Vec::new(),
            counts: Vec::new(),
        });
        let mut counts = vec![zero_counts()];
        if key.iter().any(|&node| self.trie[node].terminal) {
            counts[0][0] = BigUint::from(1u8);
        }
        let mut destinations: BTreeMap<u8, Vec<usize>> = BTreeMap::new();
        for &node in &key {
            for (&letter, &child) in &self.trie[node].children {
                for byte in alternatives(letter, self.settings) {
                    destinations.entry(byte).or_default().push(child);
                }
            }
        }
        let mut edges = Vec::new();
        for (byte, mut children) in destinations {
            children.sort_unstable();
            children.dedup();
            let changed = usize::from(self.tracked && !byte.is_ascii_lowercase());
            let next = self.state(children)?;
            let bit = class(byte);
            let child_counts = &self.states[next].counts;
            counts.resize_with(
                counts
                    .len()
                    .max((child_counts.len() + changed).min(self.limit + 1)),
                zero_counts,
            );
            for (suffix_changes, by_mask) in child_counts
                .iter()
                .enumerate()
                .take(self.limit + 1 - changed)
            {
                for (mask, count) in by_mask.iter().enumerate() {
                    counts[suffix_changes + changed][mask | bit] += count;
                }
            }
            edges.push(Edge { byte, next });
        }
        self.states[id] = State { edges, counts };
        Ok(id)
    }
}

impl WordVariants {
    fn new(
        words: &[&str],
        settings: Complexity,
        limit: usize,
        tracked: bool,
    ) -> Result<Self, Error> {
        let length = words[0].len();
        let mut builder = WordBuilder {
            trie: vec![TrieNode::default()],
            states: Vec::new(),
            memo: HashMap::new(),
            settings,
            limit,
            tracked,
        };
        for word in words {
            let mut node = 0;
            for byte in word.bytes() {
                let next = if let Some(&child) = builder.trie[node].children.get(&byte) {
                    child
                } else {
                    let child = builder.trie.len();
                    builder.trie.push(TrieNode::default());
                    builder.trie[node].children.insert(byte, child);
                    child
                };
                node = next;
            }
            builder.trie[node].terminal = true;
        }
        let root = builder.state(vec![0])?;
        let mut allowed_counts = builder.states[root].counts.clone();
        for counts in &mut allowed_counts {
            for bit in [LOWER, UPPER, DIGIT, SPECIAL] {
                for mask in 0..MASKS {
                    if mask & bit != 0 {
                        let add = counts[mask ^ bit].clone();
                        counts[mask] += add;
                    }
                }
            }
        }
        Ok(Self {
            length,
            states: builder.states,
            root,
            tracked,
            allowed_counts,
        })
    }

    fn counts(&self) -> &[Counts] {
        &self.states[self.root].counts
    }

    fn generate(
        &self,
        mask: usize,
        mut changes: usize,
        random: &mut impl RandomSource,
    ) -> Result<String, Error> {
        let mut ticket = random_below(random, &self.counts()[changes][mask])?;
        let mut state = self.root;
        let mut seen = 0;
        let mut output = String::with_capacity(self.length);
        for _ in 0..self.length {
            let mut selected = None;
            for edge in &self.states[state].edges {
                let next_seen = seen | class(edge.byte);
                if next_seen & !mask != 0 {
                    continue;
                }
                let changed = usize::from(self.tracked && !edge.byte.is_ascii_lowercase());
                if changed > changes {
                    continue;
                }
                let Some(counts) = self.states[edge.next].counts.get(changes - changed) else {
                    continue;
                };
                let mass: BigUint = counts
                    .iter()
                    .enumerate()
                    .filter(|(suffix, _)| next_seen | suffix == mask)
                    .map(|(_, count)| count)
                    .sum();
                if ticket < mass {
                    selected = Some((edge, next_seen));
                    break;
                }
                ticket -= mass;
            }
            let (edge, next_seen) = selected.expect("ticket selects a valid rendering");
            changes -= usize::from(self.tracked && !edge.byte.is_ascii_lowercase());
            output.push(char::from(edge.byte));
            state = edge.next;
            seen = next_seen;
        }
        debug_assert_eq!(seen, mask);
        Ok(output)
    }
}

pub(crate) struct ScramblePlan {
    groups: Vec<WordVariants>,
    // Suffix counts within word, length, and change caps, indexed by missing classes.
    rows: Vec<Vec<Vec<Counts>>>,
    max_words: usize,
    max_changes: usize,
    required: usize,
    outcomes: BigUint,
}

impl ScramblePlan {
    pub(crate) fn new(
        generator: &Generator,
        max_words: usize,
        effective_length: usize,
    ) -> Result<Self, Error> {
        let settings = generator.complexity;
        let minimum = usize::from(settings.uppercase)
            + usize::from(settings.digit)
            + usize::from(settings.special);
        let tracked = settings.amount < 100;
        let max_changes = if tracked {
            minimum + (effective_length - minimum) * usize::from(settings.amount) / 100
        } else {
            0
        };
        let mut groups = Vec::new();
        let mut start = 0;
        let mut states = 0;
        while start < generator.words.len() {
            let length = generator.words[start].len();
            let end = generator.words[start..].partition_point(|word| word.len() == length) + start;
            let group = WordVariants::new(
                &generator.words[start..end],
                settings,
                if tracked { max_changes } else { length },
                tracked,
            )?;
            states += group.states.len();
            if states > MAX_STATES {
                return Err(Error::PlanningLimit);
            }
            groups.push(group);
            start = end;
        }
        let required = LOWER
            | if settings.uppercase { UPPER } else { 0 }
            | if settings.digit { DIGIT } else { 0 }
            | if settings.special { SPECIAL } else { 0 };
        let separator_len = generator.separator.len();
        let shortest = groups[0].length;
        let longest_cost = groups.last().unwrap().length.saturating_add(separator_len);
        let max_tail_budget = generator.max_len - shortest;
        let mut rows = vec![vec![vec![allowed_stop_counts()]]];
        let mut cells = MASKS;
        for count in 1..max_words {
            let horizon = longest_cost.saturating_mul(count).min(max_tail_budget);
            let mut row = Vec::with_capacity(horizon + 1);
            for budget in 0..=horizon {
                let cap = max_changes.min(budget);
                cells += (cap + 1) * MASKS;
                if cells > MAX_CELLS {
                    return Err(Error::PlanningLimit);
                }
                let mut by_changes = Vec::with_capacity(cap + 1);
                for changes in 0..=cap {
                    let mut total = allowed_stop_counts();
                    for group in &groups {
                        let Some(cost) = group.length.checked_add(separator_len) else {
                            break;
                        };
                        if cost > budget {
                            break;
                        }
                        let previous = &rows[count - 1];
                        let suffix = &previous[(budget - cost).min(previous.len() - 1)];
                        for (used, by_allowed) in
                            group.allowed_counts.iter().enumerate().take(changes + 1)
                        {
                            let suffix = &suffix[(changes - used).min(suffix.len() - 1)];
                            for allowed in 0..MASKS {
                                if by_allowed[allowed] != BigUint::default()
                                    && suffix[allowed] != BigUint::default()
                                {
                                    total[allowed] += &by_allowed[allowed] * &suffix[allowed];
                                }
                            }
                        }
                    }
                    by_changes.push(total);
                }
                row.push(by_changes);
            }
            rows.push(row);
        }
        // Convert counts restricted to allowed classes into counts that require
        // each missing class. Subset transforms avoid a mask convolution in the
        // much larger length/word/change dynamic program.
        for row in &mut rows {
            for by_changes in row {
                for counts in by_changes {
                    for bit in [LOWER, UPPER, DIGIT, SPECIAL] {
                        for mask in 0..MASKS {
                            if mask & bit != 0 {
                                let subtract = counts[mask ^ bit].clone();
                                counts[mask] -= subtract;
                            }
                        }
                    }
                    for bit in [LOWER, UPPER, DIGIT, SPECIAL] {
                        for mask in 0..MASKS {
                            if mask & bit == 0 {
                                let add = counts[mask | bit].clone();
                                counts[mask] += add;
                            }
                        }
                    }
                }
            }
        }
        let mut plan = Self {
            groups,
            rows,
            max_words,
            max_changes,
            required,
            outcomes: BigUint::default(),
        };
        for group in &plan.groups {
            for (changes, by_mask) in group.counts().iter().enumerate().take(max_changes + 1) {
                for (mask, variants) in by_mask.iter().enumerate() {
                    plan.outcomes += variants
                        * plan.completions(
                            max_words - 1,
                            generator.max_len - group.length,
                            max_changes - changes,
                            required & !mask,
                        );
                }
            }
        }
        if plan.outcomes == BigUint::default() {
            return Err(Error::NoValidScramble);
        }
        Ok(plan)
    }

    fn completions(&self, words: usize, budget: usize, changes: usize, need: usize) -> &BigUint {
        let row = &self.rows[words];
        let cell = &row[budget.min(row.len() - 1)];
        &cell[changes.min(cell.len() - 1)][need]
    }

    pub(crate) fn sequence_count(&self) -> &BigUint {
        &self.outcomes
    }

    pub(crate) fn generate(
        &self,
        generator: &Generator,
        random: &mut impl RandomSource,
    ) -> Result<String, Error> {
        let mut password = String::new();
        let mut budget = generator.max_len;
        let mut changes_left = self.max_changes;
        let mut words = self.max_words;
        let mut need = self.required;
        let mut first = true;
        while words > 0 {
            let total = if first {
                &self.outcomes
            } else {
                self.completions(words, budget, changes_left, need)
            };
            let mut ticket = random_below(random, total)?;
            if !first && need == 0 {
                if ticket == BigUint::default() {
                    break;
                }
                ticket -= 1u8;
            }
            let separator_len = if first { 0 } else { generator.separator.len() };
            let mut selected = None;
            'groups: for (index, group) in self.groups.iter().enumerate() {
                let Some(cost) = group.length.checked_add(separator_len) else {
                    break;
                };
                if cost > budget {
                    break;
                }
                for (changes, by_mask) in group.counts().iter().enumerate().take(changes_left + 1) {
                    for (mask, variants) in by_mask.iter().enumerate() {
                        let mass = variants
                            * self.completions(
                                words - 1,
                                budget - cost,
                                changes_left - changes,
                                need & !mask,
                            );
                        if ticket < mass {
                            selected = Some((index, mask, cost, changes));
                            break 'groups;
                        }
                        ticket -= mass;
                    }
                }
            }
            let (index, mask, cost, changes) = selected.expect("ticket selects a valid sequence");
            let word = self.groups[index].generate(mask, changes, random)?;
            if !first {
                password.push_str(&generator.separator);
            }
            password.push_str(&word);
            first = false;
            budget -= cost;
            need &= !mask;
            changes_left -= changes;
            words -= 1;
        }
        debug_assert_eq!(need, 0);
        Ok(password)
    }
}

fn allowed_stop_counts() -> Counts {
    std::array::from_fn(|_| BigUint::from(1u8))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;
    use std::num::NonZeroUsize;

    struct Ticket(u64);
    impl RandomSource for Ticket {
        fn next_u64(&mut self) -> Result<u64, getrandom::Error> {
            Ok(self.0)
        }
    }

    // Independent exhaustive oracle with literal choices, not the production
    // automaton or its transition builder. Deliberately include i/l collisions.
    fn render(word: &str, cap: usize) -> HashSet<String> {
        let mut outputs = HashSet::from([String::new()]);
        for letter in word.chars() {
            let choices = match letter {
                'e' => "eE3#@",
                'i' => "iI1!",
                'l' => "lL1!_",
                't' => "tT7",
                _ => panic!("unsupported oracle letter"),
            };
            outputs = outputs
                .into_iter()
                .flat_map(|prefix| {
                    choices
                        .chars()
                        .map(move |choice| format!("{prefix}{choice}"))
                })
                .filter(|value| {
                    value
                        .bytes()
                        .filter(|byte| !byte.is_ascii_lowercase())
                        .count()
                        <= cap
                })
                .collect();
        }
        outputs
    }

    fn mask(value: &str) -> usize {
        value.bytes().fold(0, |mask, byte| mask | class(byte))
    }

    fn all_classes(amount: u8) -> Complexity {
        Complexity {
            uppercase: true,
            digit: true,
            special: true,
            amount,
        }
    }

    #[test]
    fn supports_multiple_alternatives_and_independent_positions_in_the_requested_examples() {
        for (word, examples) in [
            (
                "elite",
                ["elite", "3lite", "3l1te", "el1te", "el1t#"].as_slice(),
            ),
            (
                "leet",
                ["leet", "l33t", "l3et", "le3t", "le#t", "le@t"].as_slice(),
            ),
        ] {
            let variants = WordVariants::new(&[word], all_classes(100), word.len(), false).unwrap();
            for &example in examples {
                let mut state = variants.root;
                for byte in example.bytes() {
                    state = variants.states[state]
                        .edges
                        .iter()
                        .find(|edge| edge.byte == byte)
                        .unwrap_or_else(|| panic!("missing variant {example}"))
                        .next;
                }
                assert_eq!(variants.states[state].counts[0][0], BigUint::from(1u8));
            }
        }
    }

    #[test]
    fn distinct_variant_counts_and_every_unranked_output_match_exhaustive_oracle() {
        let words = ["eile", "elle"];
        let expected: HashSet<_> = words.iter().flat_map(|word| render(word, 3)).collect();
        let variants = WordVariants::new(&words, all_classes(0), 3, true).unwrap();
        let mut actual = HashSet::new();
        for changes in 0..variants.counts().len() {
            for wanted in 0..MASKS {
                let count = variants.counts()[changes][wanted]
                    .to_u64_digits()
                    .first()
                    .copied()
                    .unwrap_or(0);
                let oracle_count = expected
                    .iter()
                    .filter(|value| {
                        mask(value) == wanted
                            && value
                                .bytes()
                                .filter(|byte| !byte.is_ascii_lowercase())
                                .count()
                                == changes
                    })
                    .count();
                assert_eq!(count as usize, oracle_count);
                for ticket in 0..count {
                    let value = variants
                        .generate(wanted, changes, &mut Ticket(ticket))
                        .unwrap();
                    assert!(
                        actual.insert(value),
                        "duplicate output for distinct tickets"
                    );
                }
            }
        }
        assert_eq!(actual, expected);
    }

    #[test]
    fn whole_password_counts_and_entropy_match_exhaustive_oracle() {
        for amount in [0, 25, 100] {
            for separator in ["-", "", "e", "1", "::"] {
                let generator = Generator {
                    words: vec!["eile", "elle"],
                    max_len: 9,
                    max_words: NonZeroUsize::new(2),
                    separator: separator.into(),
                    complexity: all_classes(amount),
                };
                let plan = generator.prepare().unwrap();
                let cap = 3 + (9 - 3) * usize::from(amount) / 100;
                let variants: HashSet<_> = ["eile", "elle"]
                    .iter()
                    .flat_map(|word| render(word, 4))
                    .collect();
                let mut outputs = HashMap::<String, usize>::new();
                for a in &variants {
                    let candidates = std::iter::once(a.clone()).chain(
                        variants
                            .iter()
                            .filter(|_| 8 + separator.len() <= 9)
                            .map(|b| format!("{a}{separator}{b}")),
                    );
                    for output in candidates {
                        // Remove fixed separator before applying the class/change oracle.
                        let letters = if output.len() == 4 {
                            output.clone()
                        } else {
                            format!("{}{}", &output[..4], &output[4 + separator.len()..])
                        };
                        let changes = letters
                            .bytes()
                            .filter(|byte| !byte.is_ascii_lowercase())
                            .count();
                        if mask(&letters) == 15 && changes <= cap {
                            *outputs.entry(output).or_default() += 1;
                        }
                    }
                }
                let sequences: usize = outputs.values().sum();
                assert_eq!(
                    plan.sequence_count(),
                    &BigUint::from(sequences),
                    "{amount} {separator:?}"
                );
                let multiplicity = *outputs.values().max().unwrap();
                let min_entropy = (sequences as f64 / multiplicity as f64).log2();
                assert!(plan.entropy().bits <= min_entropy + 1e-10);
                if !plan.entropy().is_lower_bound {
                    assert!((plan.entropy().bits - min_entropy).abs() < 1e-10);
                }
            }
        }
    }

    #[test]
    fn broader_slider_settings_include_more_variants_and_credit_added_entropy() {
        let generator = Generator {
            words: vec!["elite"],
            max_len: 5,
            max_words: NonZeroUsize::new(1),
            separator: "-".into(),
            complexity: all_classes(0),
        };
        let minimal_plan = generator.prepare().unwrap();
        let minimal_count = minimal_plan.sequence_count().clone();
        let minimal_bits = minimal_plan.entropy().bits;
        drop(minimal_plan);
        let broader_generator = generator.with_complexity(all_classes(100));
        let broader = broader_generator.prepare().unwrap();
        assert!(broader.sequence_count() > &minimal_count);
        assert!(minimal_bits > 0.0); // Word alone contributes zero bits.
        assert!(broader.entropy().bits > minimal_bits);
    }

    #[test]
    fn every_class_combination_is_sparse_at_minimum_and_random_in_position() {
        for flags in 1u8..8 {
            let complexity = Complexity {
                uppercase: flags & 1 != 0,
                digit: flags & 2 != 0,
                special: flags & 4 != 0,
                amount: 0,
            };
            let generator = Generator {
                words: vec!["elite"],
                max_len: 11,
                max_words: NonZeroUsize::new(2),
                separator: "-".into(),
                complexity,
            };
            let plan = generator.prepare().unwrap();
            let minimum = flags.count_ones() as usize;
            let mut outputs = HashSet::new();
            for _ in 0..48 {
                let output = plan.generate().unwrap();
                let letters: Vec<_> = output.bytes().filter(|&byte| byte != b'-').collect();
                assert_eq!(
                    letters
                        .iter()
                        .filter(|byte| !byte.is_ascii_lowercase())
                        .count(),
                    minimum
                );
                assert_eq!(
                    letters.iter().any(|byte| byte.is_ascii_uppercase()),
                    complexity.uppercase
                );
                assert_eq!(
                    letters.iter().any(|byte| byte.is_ascii_digit()),
                    complexity.digit
                );
                assert_eq!(
                    letters
                        .iter()
                        .any(|byte| crate::SPECIAL_CHARACTERS.as_bytes().contains(byte)),
                    complexity.special
                );
                assert!(letters.iter().any(|byte| byte.is_ascii_lowercase()));
                assert!(output.len() <= 11);
                outputs.insert(output);
            }
            assert!(outputs.len() > 1);
        }
    }

    #[test]
    fn impossible_constraints_and_invalid_slider_fail_cleanly() {
        let generator = Generator {
            words: vec!["xxxx"],
            max_len: 4,
            max_words: None,
            separator: "-".into(),
            complexity: all_classes(0),
        };
        assert!(matches!(generator.prepare(), Err(Error::NoValidScramble)));
        let generator = generator.with_complexity(Complexity {
            amount: 101,
            ..Complexity::default()
        });
        assert!(matches!(
            generator.prepare(),
            Err(Error::InvalidScrambleAmount)
        ));
    }

    #[test]
    fn secure_randomness_failure_never_returns_a_partial_scrambled_password() {
        struct Failed;
        impl RandomSource for Failed {
            fn next_u64(&mut self) -> Result<u64, getrandom::Error> {
                Err(getrandom::Error::UNSUPPORTED)
            }
        }
        let generator = Generator {
            words: vec!["elite"],
            max_len: 5,
            max_words: None,
            separator: "-".into(),
            complexity: all_classes(0),
        };
        let plan = ScramblePlan::new(&generator, 1, 5).unwrap();
        assert!(matches!(
            plan.generate(&generator, &mut Failed),
            Err(Error::Randomness(_))
        ));
    }
}
