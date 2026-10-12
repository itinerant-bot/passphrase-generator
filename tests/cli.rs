use std::collections::HashSet;
use std::process::{Command, Output};

fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_four-words"))
        .args(args)
        .output()
        .expect("could not run four-words")
}

fn selected_words(selection: &str) -> HashSet<&'static str> {
    let mut words = HashSet::new();
    for (letter, source) in [
        ('e', include_str!("../wordlists/english.txt")),
        ('f', include_str!("../wordlists/french.txt")),
        ('g', include_str!("../wordlists/german.txt")),
    ] {
        if selection.contains(letter) {
            words.extend(source.lines());
        }
    }
    words
}

fn reported_entropy(output: &Output) -> f64 {
    let reports: Vec<_> = std::str::from_utf8(&output.stderr)
        .unwrap()
        .lines()
        .collect();
    let passwords: Vec<_> = std::str::from_utf8(&output.stdout)
        .unwrap()
        .lines()
        .collect();
    assert_eq!(reports.len(), passwords.len() + 1);
    let bits: f64 = reports[0]
        .strip_prefix("Entropy: ")
        .unwrap()
        .strip_suffix(" bits")
        .unwrap()
        .parse()
        .unwrap();
    assert!(bits.is_finite() && bits >= 0.0);
    for (report, password) in reports[1..].iter().zip(passwords) {
        let length: usize = report
            .strip_prefix("Length: ")
            .unwrap()
            .strip_suffix(" characters")
            .unwrap()
            .parse()
            .unwrap();
        assert_eq!(length, password.len());
    }
    bits
}

#[test]
fn every_language_combination_obeys_the_budget_and_uses_only_selected_words() {
    for selection in ["e", "f", "g", "ef", "eg", "fg", "efg"] {
        let dictionary = selected_words(selection);
        let minimum = dictionary.iter().map(|word| word.len()).min().unwrap();
        for budget in [4, 5, 8, 9, 13, 31, 63, 64] {
            let length = budget.to_string();
            let output = run(&["--dict", selection, "--len", &length, "--suggestions", "8"]);
            if budget < minimum {
                assert!(!output.status.success());
                assert!(output.stdout.is_empty());
                assert!(
                    String::from_utf8(output.stderr)
                        .unwrap()
                        .contains("no words")
                );
                continue;
            }
            assert!(
                output.status.success(),
                "dict={selection}, len={budget}: {output:?}"
            );
            reported_entropy(&output);
            let text = String::from_utf8(output.stdout).unwrap();
            let lines: Vec<_> = text.lines().collect();
            assert_eq!(lines.len(), 8);
            for password in lines {
                assert!(!password.is_empty());
                assert!(password.is_ascii());
                assert!(password.len() <= budget);
                for word in password.split('-') {
                    assert!(word.len() >= 4);
                    assert!(dictionary.contains(word), "{selection}: {word}");
                }
            }
        }
    }
}

#[test]
fn count_limits_words_in_each_password_and_suggestions_limits_output_lines() {
    for cap in [1, 2, 5] {
        for budget in [4, 9, 63, 200] {
            let count = cap.to_string();
            let length = budget.to_string();
            let output = run(&["--count", &count, "--len", &length, "--suggestions", "5"]);
            assert!(output.status.success(), "{:?}", output);
            reported_entropy(&output);
            let text = String::from_utf8(output.stdout).unwrap();
            assert_eq!(text.lines().count(), 5);
            for password in text.lines() {
                let words = password.split('-').count();
                assert!(words >= 1 && words <= cap);
                assert!(password.len() <= budget);
            }
        }
    }
    let output = run(&["--count", "5"]);
    assert!(output.status.success());
    assert_eq!(String::from_utf8(output.stdout).unwrap().lines().count(), 1);
}

#[test]
fn separators_include_spaces_empty_strings_and_multiple_characters() {
    let dictionary = selected_words("efg");
    for separator in [" ", "", "::", "_", "--", "~"] {
        let output = run(&[
            "--len",
            "31",
            "--separator",
            separator,
            "--suggestions",
            "5",
        ]);
        assert!(output.status.success(), "{:?}", output);
        reported_entropy(&output);
        let text = String::from_utf8(output.stdout).unwrap();
        assert_eq!(text.lines().count(), 5);
        for password in text.lines() {
            assert!(password.len() <= 31);
            if separator.is_empty() {
                assert!(password.bytes().all(|byte| byte.is_ascii_lowercase()));
            } else {
                for word in password.split(separator) {
                    assert!(dictionary.contains(word), "{word:?}");
                }
            }
        }
    }
}

#[test]
fn entropy_uses_the_word_space_and_treats_the_separator_as_fixed() {
    let dictionary_size = selected_words("e").len();
    let expected = (dictionary_size as f64).log2();
    for separator in ["-", " ", "!", "::", "", "abc"] {
        let output = run(&[
            "--dict",
            "e",
            "--len",
            "63",
            "--count",
            "1",
            "--separator",
            separator,
        ]);
        assert!(output.status.success(), "{output:?}");
        assert!((reported_entropy(&output) - expected).abs() <= 0.005);
    }
    let output = run(&["--dict", "e", "--len", "4"]);
    let four_letter_words = selected_words("e")
        .iter()
        .filter(|word| word.len() == 4)
        .count();
    assert!((reported_entropy(&output) - (four_letter_words as f64).log2()).abs() <= 0.005);
}

#[test]
fn concatenation_reports_entropy_and_length_with_the_same_compact_format() {
    let output = run(&["--dict", "e", "--len", "31", "--separator", ""]);
    assert!(output.status.success());
    let reported = reported_entropy(&output);
    let generator = four_words::Generator::new("e".parse().unwrap(), 31, "").unwrap();
    let bound = generator.prepare().unwrap().entropy().bits;
    assert!(reported <= bound);
    assert!(bound - reported < 0.01);
}

#[test]
fn invalid_arguments_fail_without_printing_a_password() {
    for args in [
        vec!["--dict", ""],
        vec!["--dict", "en"],
        vec!["--dict", "x"],
        vec!["--len", "0"],
        vec!["--len", "3"],
        vec!["--len", "abc"],
        vec!["--len", "-1"],
        vec!["--count", "0"],
        vec!["--count", "abc"],
        vec!["--suggestions", "0"],
        vec!["--separator", "é"],
        vec!["--separator", "\n"],
        vec!["--separator", "\t"],
        vec!["--unknown"],
    ] {
        let output = run(&args);
        assert!(!output.status.success(), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert!(!output.stderr.is_empty(), "{args:?}");
    }
}
