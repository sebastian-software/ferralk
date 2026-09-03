#![forbid(unsafe_code)]

use std::{
    collections::{BTreeMap, BTreeSet},
    env,
    fmt::Write as _,
    fs,
    process::ExitCode,
};

#[derive(Debug, Clone, PartialEq, Eq)]
struct Measurement {
    name: String,
    nanoseconds: u64,
}

fn parse_measurements(input: &str) -> Result<Vec<Measurement>, String> {
    let mut measurements = Vec::new();
    let mut names = BTreeSet::new();

    for line in input.lines() {
        let Some(line) = line.strip_prefix("test ") else {
            continue;
        };
        let Some((name, reading)) = line.split_once(" ... bench:") else {
            continue;
        };
        let Some((nanoseconds, _spread)) = reading.trim().split_once(" ns/iter ") else {
            continue;
        };
        let nanoseconds = nanoseconds
            .replace(',', "")
            .parse::<u64>()
            .map_err(|error| format!("invalid ns/iter value for benchmark {name:?}: {error}"))?;
        if !names.insert(name.to_owned()) {
            return Err(format!("duplicate benchmark measurement {name:?}"));
        }
        measurements.push(Measurement {
            name: name.to_owned(),
            nanoseconds,
        });
    }

    if measurements.is_empty() {
        return Err("benchmark output contained no ns/iter measurements".to_owned());
    }
    Ok(measurements)
}

fn format_integer(value: u64) -> String {
    let digits = value.to_string();
    let mut formatted = String::with_capacity(digits.len() + digits.len() / 3);
    for (index, byte) in digits.bytes().enumerate() {
        if index > 0 && (digits.len() - index).is_multiple_of(3) {
            formatted.push(',');
        }
        formatted.push(char::from(byte));
    }
    formatted
}

fn markdown_name(name: &str) -> String {
    name.replace('`', "'").replace('|', "\\|")
}

fn comparison_table(base: &[Measurement], head: &[Measurement]) -> String {
    let base_by_name = base
        .iter()
        .map(|measurement| (measurement.name.as_str(), measurement.nanoseconds))
        .collect::<BTreeMap<_, _>>();
    let head_by_name = head
        .iter()
        .map(|measurement| (measurement.name.as_str(), measurement.nanoseconds))
        .collect::<BTreeMap<_, _>>();
    let mut names = base
        .iter()
        .map(|measurement| measurement.name.as_str())
        .collect::<Vec<_>>();
    names.extend(
        head.iter()
            .map(|measurement| measurement.name.as_str())
            .filter(|name| !base_by_name.contains_key(name)),
    );

    let mut table = String::from(
        "| Benchmark | Merge base ns/iter | PR head ns/iter | Head / base |\n\
         | --- | ---: | ---: | ---: |\n",
    );
    for name in names {
        let base_value = base_by_name.get(name).copied();
        let head_value = head_by_name.get(name).copied();
        let ratio = match (base_value, head_value) {
            (Some(base), Some(head)) if base > 0 => format!("{:.3}x", head as f64 / base as f64),
            _ => "n/a".to_owned(),
        };
        writeln!(
            table,
            "| `{}` | {} | {} | {ratio} |",
            markdown_name(name),
            base_value.map_or_else(|| "n/a".to_owned(), format_integer),
            head_value.map_or_else(|| "n/a".to_owned(), format_integer),
        )
        .expect("writing to a string cannot fail");
    }
    table
}

fn run() -> Result<(), String> {
    let mut arguments = env::args().skip(1);
    let base_path = arguments
        .next()
        .ok_or_else(|| "usage: compare_bencher <merge-base-output> <head-output>".to_owned())?;
    let head_path = arguments
        .next()
        .ok_or_else(|| "usage: compare_bencher <merge-base-output> <head-output>".to_owned())?;
    if arguments.next().is_some() {
        return Err("usage: compare_bencher <merge-base-output> <head-output>".to_owned());
    }

    let base = fs::read_to_string(&base_path)
        .map_err(|error| format!("failed to read {base_path:?}: {error}"))?;
    let head = fs::read_to_string(&head_path)
        .map_err(|error| format!("failed to read {head_path:?}: {error}"))?;
    let base = parse_measurements(&base)?;
    let head = parse_measurements(&head)?;
    print!("{}", comparison_table(&base, &head));
    Ok(())
}

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("compare_bencher: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_bencher_measurements_and_ignores_other_output() {
        let input = "noise\n\
            test walker/serial ... bench:       1,234 ns/iter (+/- 56)\n\
            test walker/parallel ... bench:         987 ns/iter (+/- 12)\n";
        assert_eq!(
            parse_measurements(input),
            Ok(vec![
                Measurement {
                    name: "walker/serial".to_owned(),
                    nanoseconds: 1_234,
                },
                Measurement {
                    name: "walker/parallel".to_owned(),
                    nanoseconds: 987,
                },
            ])
        );
    }

    #[test]
    fn rejects_silent_and_duplicate_benchmark_output() {
        assert!(parse_measurements("no measurements").is_err());
        assert!(
            parse_measurements(
                "test walker/a ... bench: 1 ns/iter (+/- 0)\n\
                 test walker/a ... bench: 2 ns/iter (+/- 0)\n"
            )
            .is_err()
        );
    }

    #[test]
    fn compares_shared_rows_and_marks_added_or_removed_rows() {
        let base = vec![
            Measurement {
                name: "walker/shared".to_owned(),
                nanoseconds: 1_000,
            },
            Measurement {
                name: "walker/removed".to_owned(),
                nanoseconds: 2_000,
            },
        ];
        let head = vec![
            Measurement {
                name: "walker/shared".to_owned(),
                nanoseconds: 1_050,
            },
            Measurement {
                name: "walker/added".to_owned(),
                nanoseconds: 900,
            },
        ];

        let table = comparison_table(&base, &head);
        assert!(table.contains("| `walker/shared` | 1,000 | 1,050 | 1.050x |"));
        assert!(table.contains("| `walker/removed` | 2,000 | n/a | n/a |"));
        assert!(table.contains("| `walker/added` | n/a | 900 | n/a |"));
    }
}
