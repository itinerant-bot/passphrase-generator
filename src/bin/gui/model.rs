use std::num::NonZeroUsize;

use four_words::{Complexity, Generator, Languages};

pub const MAX_LENGTH: usize = 128;
pub const MAX_WORDS: usize = 25;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    pub languages: [bool; 3],
    pub max_length: usize,
    pub limit_words: bool,
    pub max_words: usize,
    pub spaces: bool,
    pub complexity: Complexity,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            languages: [true, false, false],
            max_length: MAX_LENGTH,
            limit_words: false,
            max_words: 5,
            spaces: false,
            complexity: Complexity::default(),
        }
    }
}

impl Settings {
    pub fn generate(&self) -> Result<Suggestion, String> {
        let selection: String = self
            .languages
            .iter()
            .zip(['e', 'f', 'g'])
            .filter_map(|(&enabled, letter)| enabled.then_some(letter))
            .collect();
        if selection.is_empty() {
            return Err("Choose at least one language.".into());
        }
        if !(4..=MAX_LENGTH).contains(&self.max_length) {
            return Err(format!("Enter a character limit from 4 to {MAX_LENGTH}."));
        }
        let languages: Languages = selection.parse().map_err(|error| format!("{error}"))?;
        let separator = if self.spaces { " " } else { "-" };
        let mut generator = Generator::new(languages, self.max_length, separator)
            .map_err(|_| "Increase the character limit so a word can fit.".to_owned())?;
        generator = generator.with_complexity(self.complexity);
        if self.limit_words {
            if !(1..=MAX_WORDS).contains(&self.max_words) {
                return Err(format!("Enter a word limit from 1 to {MAX_WORDS}."));
            }
            generator = generator.with_max_words(NonZeroUsize::new(self.max_words).unwrap());
        }
        let plan = generator.prepare().map_err(|error| error.to_string())?;
        let password = plan.generate().map_err(|error| error.to_string())?;
        let words = password.split(separator).count();
        Ok(Suggestion {
            bits: plan.entropy().bits,
            password,
            words,
        })
    }
}

#[derive(Debug)]
pub struct Suggestion {
    pub bits: f64,
    pub password: String,
    pub words: usize,
}

pub struct NumberInput {
    pub value: usize,
    pub text: String,
    pub min: usize,
    pub max: usize,
}

impl NumberInput {
    pub fn new(value: usize, min: usize, max: usize) -> Self {
        Self {
            value,
            text: value.to_string(),
            min,
            max,
        }
    }

    pub fn parsed(&self) -> Option<usize> {
        self.text
            .trim()
            .parse::<usize>()
            .ok()
            .filter(|value| (self.min..=self.max).contains(value))
    }
}

pub struct Band {
    pub start: f64,
    pub label: &'static str,
    pub rgb: [u8; 3],
}

pub const BANDS: [Band; 5] = [
    Band {
        start: 0.0,
        label: "Insecure",
        rgb: [220, 20, 60],
    },
    Band {
        start: 55.0,
        label: "Standard consumer",
        rgb: [255, 191, 0],
    },
    Band {
        start: 65.0,
        label: "Strong protection",
        rgb: [80, 200, 120],
    },
    Band {
        start: 80.0,
        label: "Master key grade",
        rgb: [15, 82, 186],
    },
    Band {
        start: 96.0,
        label: "Password manager grade",
        rgb: [143, 0, 255],
    },
];

pub fn band_index(bits: f64) -> usize {
    BANDS
        .iter()
        .rposition(|band| bits >= band.start)
        .unwrap_or(0)
}

/// Anchor bounded ranges at their midpoints. The open upper range is violet
/// from 96 bits onward; below the first midpoint remains crimson.
pub fn entropy_rgb(bits: f64) -> [u8; 3] {
    const MIDPOINTS: [f64; 5] = [27.5, 60.0, 72.5, 88.0, 96.0];
    let upper = MIDPOINTS.iter().position(|midpoint| bits < *midpoint);
    let Some(upper) = upper else {
        return BANDS[4].rgb;
    };
    if upper == 0 {
        return BANDS[0].rgb;
    }
    let lower = upper - 1;
    let fraction =
        ((bits - MIDPOINTS[lower]) / (MIDPOINTS[upper] - MIDPOINTS[lower])).clamp(0.0, 1.0);
    std::array::from_fn(|channel| {
        (f64::from(BANDS[lower].rgb[channel])
            + fraction
                * (f64::from(BANDS[upper].rgb[channel]) - f64::from(BANDS[lower].rgb[channel])))
        .round() as u8
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifications_have_no_gaps_and_gradient_matches_range_midpoints() {
        for (bits, index) in [
            (0.0, 0),
            (54.99, 0),
            (55.0, 1),
            (64.99, 1),
            (65.0, 2),
            (79.99, 2),
            (80.0, 3),
            (95.99, 3),
            (96.0, 4),
            (400.0, 4),
        ] {
            assert_eq!(band_index(bits), index);
        }
        for (bits, index) in [(27.5, 0), (60.0, 1), (72.5, 2), (88.0, 3), (96.0, 4)] {
            assert_eq!(entropy_rgb(bits), BANDS[index].rgb);
        }
        assert_eq!(entropy_rgb(0.0), BANDS[0].rgb);
        assert_eq!(entropy_rgb(248.52), BANDS[4].rgb);
        for bits in [55.0, 65.0, 80.0] {
            let before = entropy_rgb(bits - 0.01);
            let after = entropy_rgb(bits + 0.01);
            for (before, after) in before.into_iter().zip(after) {
                assert!(
                    before.abs_diff(after) <= 1,
                    "gradient jumps at a classification boundary"
                );
            }
        }
    }

    #[test]
    fn editable_values_reject_partial_and_out_of_range_input() {
        let mut input = NumberInput::new(128, 4, MAX_LENGTH);
        assert_eq!(input.parsed(), Some(128));
        for value in ["", "hello", "3", "129", "-1", "12.5"] {
            input.text = value.into();
            assert_eq!(input.parsed(), None);
        }
        input.text = " 63 ".into();
        assert_eq!(input.parsed(), Some(63));
    }

    #[test]
    fn word_checkbox_changes_entropy_and_generation_constraints() {
        let mut settings = Settings {
            max_words: 1,
            limit_words: true,
            ..Settings::default()
        };
        let limited = settings.generate().unwrap();
        assert_eq!(limited.words, 1);
        settings.limit_words = false;
        let unlimited = settings.generate().unwrap();
        assert!(unlimited.bits > limited.bits);
        assert!(unlimited.password.len() <= MAX_LENGTH);
        settings.max_words = 0; // The disabled setting is ignored.
        assert!(settings.generate().is_ok());
    }

    #[test]
    fn gui_uses_same_entropy_as_cli_and_counts_length_including_separator() {
        let settings = Settings {
            languages: [true, true, false],
            max_length: 63,
            max_words: 5,
            spaces: true,
            complexity: Complexity::default(),
            limit_words: true,
        };
        let result = settings.generate().unwrap();
        let generator = Generator::new("ef".parse().unwrap(), 63, " ")
            .unwrap()
            .with_max_words(NonZeroUsize::new(5).unwrap());
        assert_eq!(result.bits, generator.prepare().unwrap().entropy().bits);
        assert!(result.password.len() <= 63);
        assert!(result.words <= 5);
        assert_eq!(result.words, result.password.split(' ').count());
    }

    #[test]
    fn website_settings_share_the_cli_scrambler_and_preserve_counts() {
        let settings = Settings {
            max_length: 5,
            max_words: 1,
            limit_words: true,
            complexity: Complexity {
                uppercase: true,
                digit: true,
                special: true,
                amount: 0,
            },
            ..Settings::default()
        };
        let result = settings.generate().unwrap();
        assert_eq!(result.words, 1);
        assert!(result.password.len() <= 5);
        assert_eq!(
            result
                .password
                .bytes()
                .filter(|byte| !byte.is_ascii_lowercase())
                .count(),
            3
        );
        let generator = Generator::new("e".parse().unwrap(), 5, "-")
            .unwrap()
            .with_max_words(NonZeroUsize::new(1).unwrap())
            .with_complexity(settings.complexity);
        assert_eq!(result.bits, generator.prepare().unwrap().entropy().bits);
    }

    #[test]
    fn unavailable_settings_produce_actionable_errors() {
        let mut settings = Settings {
            languages: [false; 3],
            ..Settings::default()
        };
        assert_eq!(
            settings.generate().unwrap_err(),
            "Choose at least one language."
        );
        settings.languages = [false, false, true];
        settings.max_length = 4;
        assert!(settings.generate().is_err());
        settings.max_length = 128;
        settings.limit_words = true;
        settings.max_words = 0;
        assert!(settings.generate().is_err());
    }
}
