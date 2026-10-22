//! Word-based password suggestions using the operating system's secure RNG.

use std::fmt;
use std::num::NonZeroUsize;
use std::str::FromStr;

mod entropy;
mod scramble;
pub use entropy::{Entropy, Plan};

const ENGLISH: &str = include_str!("../wordlists/english.txt");
const FRENCH: &str = include_str!("../wordlists/french.txt");
const GERMAN: &str = include_str!("../wordlists/german.txt");
const SPANISH: &str = include_str!("../wordlists/spanish.txt");
const LATIN: &str = include_str!("../wordlists/latin.txt");
const MIN_WORD_LEN: usize = 4;

/// Limited punctuation set used by the optional presentation transformation.
pub const SPECIAL_CHARACTERS: &str = "!@#%_";

/// Character classes to guarantee after selecting words. Dictionaries are unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Complexity {
    pub uppercase: bool,
    pub digit: bool,
    pub special: bool,
    /// Maximum changed characters per password, from minimal (0) to all (100).
    pub amount: u8,
}

impl Complexity {
    pub(crate) fn enabled(self) -> bool {
        self.uppercase || self.digit || self.special
    }
}

/// A nonempty set of languages, selected by any combination of `e`, `f`, `g`, `s`, and `l`.
/// Repeated letters select a language only once.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Languages(u8);

impl FromStr for Languages {
    type Err = Error;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let mut mask = 0;
        for letter in value.chars() {
            mask |= match letter {
                'e' => 1,
                'f' => 2,
                'g' => 4,
                's' => 8,
                'l' => 16,
                _ => return Err(Error::InvalidLanguages(value.to_owned())),
            };
        }
        if mask == 0 {
            return Err(Error::InvalidLanguages(value.to_owned()));
        }
        Ok(Self(mask))
    }
}

#[derive(Debug)]
pub enum Error {
    InvalidLanguages(String),
    LengthTooSmall(usize),
    InvalidSeparator,
    NoWordsFit,
    Randomness(getrandom::Error),
    PlanningLimit,
    InvalidScrambleAmount,
    NoValidScramble,
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLanguages(value) => write!(
                f,
                "invalid dictionary {value:?}; use a nonempty combination of e, f, g, s, and l"
            ),
            Self::LengthTooSmall(length) => write!(
                f,
                "maximum length {length} is too small; words need at least {MIN_WORD_LEN} characters"
            ),
            Self::InvalidSeparator => {
                write!(
                    f,
                    "separator must contain only printable ASCII characters or spaces"
                )
            }
            Self::NoWordsFit => write!(
                f,
                "no words in the selected dictionary fit the length limit"
            ),
            Self::Randomness(error) => write!(f, "secure OS randomness is unavailable: {error}"),
            Self::InvalidScrambleAmount => write!(f, "scramble amount must be from 0 to 100"),
            Self::NoValidScramble => write!(
                f,
                "no readable transformations satisfy the selected character classes within these limits; increase the length or word limit"
            ),
            Self::PlanningLimit => write!(
                f,
                "constraints exceed the entropy planner's resource limit; reduce --len or --count"
            ),
        }
    }
}

impl std::error::Error for Error {}

/// Reusable generator with an embedded, deduplicated dictionary.
pub struct Generator {
    words: Vec<&'static str>,
    max_len: usize,
    max_words: Option<NonZeroUsize>,
    separator: String,
    complexity: Complexity,
}

impl Generator {
    /// Construct a generator. The length includes every separator, and the
    /// separator may be empty. Source words are always lowercase ASCII letters.
    pub fn new(languages: Languages, max_len: usize, separator: &str) -> Result<Self, Error> {
        if max_len < MIN_WORD_LEN {
            return Err(Error::LengthTooSmall(max_len));
        }
        if !separator.bytes().all(|byte| (b' '..=b'~').contains(&byte)) {
            return Err(Error::InvalidSeparator);
        }

        let mut words = dictionary(languages);
        words.retain(|word| word.len() <= max_len);
        if words.is_empty() {
            return Err(Error::NoWordsFit);
        }
        Ok(Self {
            words,
            max_len,
            max_words: None,
            separator: separator.to_owned(),
            complexity: Complexity::default(),
        })
    }

    /// Limit the number of appended words. The length limit still takes
    /// precedence, so a password may contain fewer words than this cap.
    pub fn with_max_words(mut self, limit: NonZeroUsize) -> Self {
        self.max_words = Some(limit);
        self
    }

    /// Apply optional randomized presentation after word selection. The original
    /// length limit is unchanged; replacements always retain lowercase letters.
    pub fn with_complexity(mut self, complexity: Complexity) -> Self {
        self.complexity = complexity;
        self
    }

    /// Generate a complete password, or return an error without returning a
    /// partial password. There is no weak RNG fallback or user-supplied seed.
    pub fn generate(&self) -> Result<String, Error> {
        self.prepare()?.generate()
    }

    /// Count all valid sequences and prepare uniform, maximum-entropy sampling.
    /// Reuse the plan when generating several suggestions with these settings.
    pub fn prepare(&self) -> Result<Plan<'_>, Error> {
        Plan::new(self)
    }
}

fn dictionary(languages: Languages) -> Vec<&'static str> {
    let mut words = Vec::new();
    for (bit, source) in [
        (1, ENGLISH),
        (2, FRENCH),
        (4, GERMAN),
        (8, SPANISH),
        (16, LATIN),
    ] {
        if languages.0 & bit != 0 {
            words.extend(source.lines().filter(|word| {
                word.len() >= MIN_WORD_LEN && word.bytes().all(|byte| byte.is_ascii_lowercase())
            }));
        }
    }
    // Shared spellings must not get extra probability just for appearing in
    // more than one language. Sorting also makes dictionary order reproducible.
    words.sort_unstable();
    words.dedup();
    words.sort_unstable_by_key(|word| (word.len(), *word));
    words
}

// This interface is private: production callers cannot inject an insecure RNG.
trait RandomSource {
    fn next_u64(&mut self) -> Result<u64, getrandom::Error>;
}

struct OsRandom;

impl RandomSource for OsRandom {
    fn next_u64(&mut self) -> Result<u64, getrandom::Error> {
        let mut bytes = [0; 8];
        getrandom::fill(&mut bytes)?;
        Ok(u64::from_ne_bytes(bytes))
    }
}

/// Sample an unbiased index in `0..bound` from secure 64-bit draws.
fn random_index(random: &mut impl RandomSource, bound: usize) -> Result<usize, Error> {
    assert!(bound > 0);
    let bound = bound as u64;
    // 2^64 % bound, expressed without overflowing. Discard this many values
    // from the bottom of the range, leaving an exact multiple of bound.
    let threshold = bound.wrapping_neg() % bound;
    loop {
        let value = random.next_u64().map_err(Error::Randomness)?;
        if value >= threshold {
            return Ok((value % bound) as usize);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::{HashSet, VecDeque};

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

    #[test]
    fn rejects_invalid_language_sets_and_deduplicates_selections() {
        for value in ["", "en", "x", "e,f", " ef", "é"] {
            assert!(value.parse::<Languages>().is_err(), "{value:?}");
        }
        assert_eq!(
            "efg".parse::<Languages>().unwrap(),
            "ggffee".parse().unwrap()
        );
        assert_eq!("ef".parse::<Languages>().unwrap(), Languages(3));
        assert_eq!("efgsl".parse::<Languages>().unwrap(), Languages(31));
        assert_eq!("llssggffee".parse::<Languages>().unwrap(), Languages(31));
    }

    #[test]
    fn embedded_lists_are_large_sorted_unique_ascii_word_dictionaries() {
        for source in [ENGLISH, FRENCH, GERMAN, SPANISH, LATIN] {
            let words: Vec<_> = source.lines().collect();
            assert!(words.len() >= 5_000);
            assert!(words.windows(2).all(|pair| pair[0] < pair[1]));
            for word in words {
                assert!(word.len() >= 4, "{word:?}");
                assert!(
                    word.bytes().all(|byte| byte.is_ascii_lowercase()),
                    "{word:?}"
                );
            }
        }
    }

    #[test]
    fn language_subsets_merge_without_extra_weight_for_shared_spellings() {
        let sources = [
            ('e', ENGLISH),
            ('f', FRENCH),
            ('g', GERMAN),
            ('s', SPANISH),
            ('l', LATIN),
        ];
        for bits in 1u8..32 {
            let selection: String = sources
                .iter()
                .enumerate()
                .filter(|(index, _)| bits & (1 << index) != 0)
                .map(|(_, (letter, _))| *letter)
                .collect();
            let expected: HashSet<_> = sources
                .iter()
                .filter(|(letter, _)| selection.contains(*letter))
                .flat_map(|(_, source)| source.lines())
                .collect();
            let actual = dictionary(selection.parse().unwrap());
            assert_eq!(actual.len(), expected.len(), "{selection}");
            assert_eq!(
                actual.into_iter().collect::<HashSet<_>>(),
                expected,
                "{selection}"
            );
        }
    }

    #[test]
    fn bounded_sampling_rejects_the_modulo_bias_region() {
        let mut random = Scripted::values(&[0, 1, 2, 3, u64::MAX]);
        // 2^64 % 3 == 1: zero must be discarded.
        assert_eq!(random_index(&mut random, 3).unwrap(), 1);
        assert_eq!(random_index(&mut random, 3).unwrap(), 2);
        assert_eq!(random_index(&mut random, 3).unwrap(), 0);
        assert_eq!(random_index(&mut random, 3).unwrap(), 0);
        assert!(random.0.is_empty());
    }

    #[test]
    fn bounded_sampling_supports_singletons_and_powers_of_two() {
        let mut random = Scripted::values(&[0, u64::MAX, 0, u64::MAX]);
        assert_eq!(random_index(&mut random, 1).unwrap(), 0);
        assert_eq!(random_index(&mut random, 1).unwrap(), 0);
        assert_eq!(random_index(&mut random, 8).unwrap(), 0);
        assert_eq!(random_index(&mut random, 8).unwrap(), 7);
    }

    #[test]
    fn rejects_invalid_limits_and_nonkeyboard_separators() {
        for length in 0..4 {
            assert!(matches!(
                Generator::new(Languages(7), length, "-"),
                Err(Error::LengthTooSmall(_))
            ));
        }
        for separator in ["é", "\n", "\t", "\u{7f}"] {
            assert!(matches!(
                Generator::new(Languages(7), 63, separator),
                Err(Error::InvalidSeparator)
            ));
        }
    }
}
