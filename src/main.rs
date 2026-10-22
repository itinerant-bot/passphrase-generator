use std::io::{self, Write};
use std::num::NonZeroUsize;
use std::process::ExitCode;

use clap::Parser;
use four_words::{Complexity, Generator, Languages};

#[derive(Parser)]
#[command(
    version,
    about = "Suggest memorable passphrases using secure OS randomness"
)]
struct Args {
    /// Dictionary languages: e = English, f = French, g = German (any combination)
    #[arg(long, default_value = "efg")]
    dict: Languages,

    /// Maximum password length, including separators (at least 4)
    #[arg(long, default_value_t = 128)]
    len: usize,

    /// Fixed printable US keyboard separator; use " " for spaces or "" to concatenate
    #[arg(long, default_value = "-", allow_hyphen_values = true)]
    separator: String,

    /// Maximum number of words per password (otherwise limited only by --len)
    #[arg(short = 'n', long)]
    count: Option<NonZeroUsize>,

    /// Randomize letter capitalization, guaranteeing uppercase and lowercase
    #[arg(long)]
    uppercase: bool,

    /// Require digits among randomized readable character substitutions
    #[arg(long)]
    digit: bool,

    /// Require special characters (!@#%_) among readable substitutions
    #[arg(long)]
    special: bool,

    /// Scramble amount for all changed letters: 0 = minimal, 100 = unconstrained
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u8).range(0..=100))]
    scramble: u8,

    /// Number of independent password suggestions to print, one per line
    #[arg(long, default_value = "1")]
    suggestions: NonZeroUsize,
}

fn run(args: Args) -> Result<(), Box<dyn std::error::Error>> {
    let mut generator =
        Generator::new(args.dict, args.len, &args.separator)?.with_complexity(Complexity {
            uppercase: args.uppercase,
            digit: args.digit,
            special: args.special,
            amount: args.scramble,
        });
    if let Some(count) = args.count {
        generator = generator.with_max_words(count);
    }
    let plan = generator.prepare()?;
    let entropy = plan.entropy();
    let bits = if entropy.is_lower_bound {
        (entropy.bits * 100.0).floor() / 100.0
    } else {
        entropy.bits
    };
    eprintln!("Entropy: {bits:.2} bits");
    let stdout = io::stdout();
    let mut output = stdout.lock();
    for _ in 0..args.suggestions.get() {
        // Generate a complete suggestion before writing anything to stdout.
        let password = plan.generate()?;
        eprintln!("Length: {} characters", password.len());
        writeln!(output, "{password}")?;
    }
    Ok(())
}

fn main() -> ExitCode {
    match run(Args::parse()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            if error
                .downcast_ref::<io::Error>()
                .is_some_and(|error| error.kind() == io::ErrorKind::BrokenPipe)
            {
                return ExitCode::SUCCESS;
            }
            eprintln!("four-words: {error}");
            ExitCode::FAILURE
        }
    }
}
