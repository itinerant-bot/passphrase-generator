//! Standalone Rust tool for reproducing the bundled dictionaries.
//! Usage: import_dictionary <upstream-list> <output-list>
use std::collections::BTreeSet;
use std::env;
use std::fs;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Vec<_> = env::args_os().skip(1).collect();
    if args.len() != 2 {
        return Err("usage: import_dictionary <upstream-list> <output-list>".into());
    }
    let source = fs::read_to_string(&args[0])?;
    let mut words = BTreeSet::new();
    for line in source.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        let word = match fields.as_slice() {
            [word] => *word,
            [roll, word] if roll.bytes().all(|byte| byte.is_ascii_digit()) => *word,
            _ => continue,
        };
        // Keep genuine ASCII spellings; do not strip accents or umlauts.
        if word.len() >= 4 && word.bytes().all(|byte| byte.is_ascii_alphabetic()) {
            words.insert(word.to_ascii_lowercase());
        }
    }
    if words.is_empty() {
        return Err("source contains no eligible words".into());
    }
    let mut output = String::new();
    for word in &words {
        output.push_str(word);
        output.push('\n');
    }
    fs::write(&args[1], output)?;
    eprintln!("imported {} unique words", words.len());
    Ok(())
}
