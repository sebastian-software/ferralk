#![forbid(unsafe_code)]
//! Reads the Callgrind totals the user-space CPU lane writes, publishes them
//! as a Markdown table, and applies the instruction-count gate.
//!
//! ```text
//! compare_callgrind --gate-percent <P> [--pull-request [--body <file>]]
//! ```
//!
//! The lane leaves `callgrind-{head,base}-{serial,parallel}.out` in the
//! working directory. On a pull request whose merge base can build the
//! harness, each arm's head count is compared with its merge-base count, and
//! the run fails when either exceeds it by more than `P` percent — unless the
//! pull-request body carries a line starting with [`ACCEPTANCE_MARKER`] and a
//! reason. `docs/benchmark-evidence.md` records how the threshold was derived.
//!
//! Instruction counts are not time. Callgrind serializes threads and does not
//! model syscall latency, so the four-thread arm reports work performed rather
//! than elapsed time, and nothing here says whether a change is faster.

use std::{env, fmt::Write as _, fs, path::Path, process::ExitCode};

/// The pull-request body line that accepts an increase over the gate. It needs
/// a reason after the colon; the bare marker accepts nothing.
const ACCEPTANCE_MARKER: &str = "CPU-Increase-Accepted:";

/// The two walks the lane measures, as `(file stem, table label)`.
const ARMS: [(&str, &str); 2] = [("serial", "1 thread"), ("parallel", "4 threads")];

/// Total instructions (`Ir`) from a Callgrind output file.
fn parse_total(callgrind: &str) -> Option<u64> {
    callgrind.lines().find_map(|line| {
        let value = line
            .strip_prefix("summary:")
            .or_else(|| line.strip_prefix("totals:"))?;
        value.split_whitespace().next()?.parse().ok()
    })
}

/// The reason given on the pull request's acceptance line, if it has one.
fn acceptance(body: &str) -> Option<&str> {
    body.lines().find_map(|line| {
        let reason = line.trim_start().strip_prefix(ACCEPTANCE_MARKER)?.trim();
        (!reason.is_empty()).then_some(reason)
    })
}

/// Parses the gate as a percentage over the merge base.
fn parse_percent(value: &str) -> Result<f64, String> {
    match value.parse::<f64>() {
        Ok(percent) if percent.is_finite() && percent > 0.0 => Ok(percent),
        _ => Err(format!(
            "the gate must be a positive percentage, not {value:?}"
        )),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Counts {
    head: Option<u64>,
    base: Option<u64>,
}

struct Report {
    markdown: String,
    failure: Option<String>,
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

/// Builds the job summary and decides whether the run fails.
///
/// `counts` is in [`ARMS`] order. `percent` is the gate as written in the
/// workflow, so the summary repeats it verbatim.
fn evaluate(
    counts: &[Counts; 2],
    percent: &str,
    pull_request: bool,
    accepted: Option<&str>,
) -> Result<Report, String> {
    let allowed = 100.0 + parse_percent(percent)?;
    let mut markdown = String::from("### User-space CPU — portable\n\n");
    let mut failures = Vec::new();

    let missing_head = ARMS
        .iter()
        .zip(counts)
        .filter(|(_, count)| count.head.is_none())
        .map(|((_, label), _)| *label)
        .collect::<Vec<_>>();
    if !missing_head.is_empty() {
        failures.push(format!(
            "Callgrind produced no head count for: {}.",
            missing_head.join(", ")
        ));
    }

    // A merge base predating the harness cannot be measured, so there is
    // nothing to gate against. A merge base that measured one arm and not the
    // other is a broken run, and a gate that quietly skipped an arm would not
    // be a gate.
    let measured_base = counts.iter().filter(|count| count.base.is_some()).count();
    let compared = measured_base > 0;
    if compared && measured_base < counts.len() {
        let missing = ARMS
            .iter()
            .zip(counts)
            .filter(|(_, count)| count.base.is_none())
            .map(|((_, label), _)| *label)
            .collect::<Vec<_>>();
        failures.push(format!(
            "The merge base produced no count for: {}.",
            missing.join(", ")
        ));
    }

    let mut over = Vec::new();
    if compared {
        markdown.push_str("| Arm | Merge base | Head | Head/base | Gate |\n");
        markdown.push_str("| --- | ---: | ---: | ---: | --- |\n");
    } else {
        markdown.push_str("| Arm | Instructions |\n| --- | ---: |\n");
    }
    for ((_, label), count) in ARMS.iter().zip(counts) {
        let head = count.head.map_or_else(|| "n/a".to_owned(), format_integer);
        if !compared {
            writeln!(markdown, "| {label} | {head} |").expect("writing to a string cannot fail");
            continue;
        }
        let base = count.base.map_or_else(|| "n/a".to_owned(), format_integer);
        let (ratio, verdict) = match (count.head, count.base) {
            (Some(head), Some(base)) if base > 0 => {
                let ratio = head as f64 / base as f64;
                // Cross-multiplied so an integral percentage compares exactly:
                // the counts stay far below 2^53 even multiplied by 100.
                let verdict = if head as f64 * 100.0 <= base as f64 * allowed {
                    format!("within {percent}%")
                } else {
                    over.push(*label);
                    if accepted.is_some() {
                        format!("over {percent}%, accepted")
                    } else {
                        format!("**over {percent}%**")
                    }
                };
                (format!("{ratio:.4}x"), verdict)
            }
            _ => ("n/a".to_owned(), "n/a".to_owned()),
        };
        writeln!(
            markdown,
            "| {label} | {base} | {head} | {ratio} | {verdict} |"
        )
        .expect("writing to a string cannot fail");
    }
    markdown.push('\n');

    if pull_request && !compared {
        markdown.push_str(
            "The merge base predates this lane, so there is nothing to compare \
             against yet and the head counts stand alone.\n\n",
        );
    }
    if !over.is_empty() {
        if let Some(reason) = accepted {
            writeln!(
                markdown,
                "Over the gate and accepted by the pull request: {reason}\n"
            )
            .expect("writing to a string cannot fail");
        } else {
            failures.push(format!(
                "The head's instruction count is more than {percent}% over the merge base \
                 on: {}. If the added work is intended, put a `{ACCEPTANCE_MARKER} <reason>` \
                 line in the pull-request body and re-run this job.",
                over.join(", ")
            ));
        }
    }

    writeln!(
        markdown,
        "Instruction counts, not time. Callgrind serializes threads and does not model \
         syscall latency, so the four-thread row reports work performed rather than \
         elapsed time. On a pull request the job fails when either arm's head count \
         exceeds its merge-base count by more than {percent}%; \
         see docs/benchmark-evidence.md for how that number was derived."
    )
    .expect("writing to a string cannot fail");
    for failure in &failures {
        writeln!(markdown, "\n{failure}").expect("writing to a string cannot fail");
    }

    Ok(Report {
        markdown,
        failure: (!failures.is_empty()).then(|| failures.join(" ")),
    })
}

fn read_total(path: &Path) -> Result<Option<u64>, String> {
    if !path.is_file() {
        return Ok(None);
    }
    let bytes = fs::read(path).map_err(|error| format!("failed to read {path:?}: {error}"))?;
    Ok(parse_total(&String::from_utf8_lossy(&bytes)))
}

const USAGE: &str = "usage: compare_callgrind --gate-percent <P> [--pull-request [--body <file>]]";

fn run() -> Result<bool, String> {
    let mut percent = None;
    let mut pull_request = false;
    let mut body_path = None;
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--gate-percent" => percent = Some(arguments.next().ok_or(USAGE)?),
            "--pull-request" => pull_request = true,
            "--body" => body_path = Some(arguments.next().ok_or(USAGE)?),
            _ => return Err(USAGE.to_owned()),
        }
    }
    let percent = percent.ok_or(USAGE)?;
    // An unreadable body only matters when the gate trips, and then it is
    // reported as "not accepted" rather than turning an API hiccup into a
    // different failure.
    let body = body_path
        .map(|path| fs::read_to_string(path).unwrap_or_default())
        .unwrap_or_default();

    let mut counts = [Counts {
        head: None,
        base: None,
    }; 2];
    for ((arm, _), count) in ARMS.iter().zip(&mut counts) {
        count.head = read_total(Path::new(&format!("callgrind-head-{arm}.out")))?;
        count.base = read_total(Path::new(&format!("callgrind-base-{arm}.out")))?;
    }

    let report = evaluate(
        &counts,
        &percent,
        pull_request,
        pull_request.then(|| acceptance(&body)).flatten(),
    )?;
    print!("{}", report.markdown);
    if let Some(failure) = report.failure {
        eprintln!("::error::{failure}");
        return Ok(false);
    }
    Ok(true)
}

fn main() -> ExitCode {
    match run() {
        Ok(true) => ExitCode::SUCCESS,
        Ok(false) => ExitCode::FAILURE,
        Err(error) => {
            eprintln!("compare_callgrind: {error}");
            ExitCode::FAILURE
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn both(head: u64, base: u64) -> Counts {
        Counts {
            head: Some(head),
            base: Some(base),
        }
    }

    #[test]
    fn parses_the_total_from_either_callgrind_footer() {
        let output = "version: 1\ncmd: cpu_walk walk /tmp/x 1\nevents: Ir\n\
                      fn=main\n1 5\nsummary: 66744131\n";
        assert_eq!(parse_total(output), Some(66_744_131));
        assert_eq!(parse_total("events: Ir\ntotals: 12 34\n"), Some(12));
        assert_eq!(parse_total("events: Ir\nfn=main\n1 5\n"), None);
    }

    #[test]
    fn acceptance_needs_the_marker_at_the_start_of_a_line_and_a_reason() {
        assert_eq!(
            acceptance(
                "## Summary\n\nCPU-Increase-Accepted: excludes now apply in hidden directories\n"
            ),
            Some("excludes now apply in hidden directories")
        );
        assert_eq!(
            acceptance("  CPU-Increase-Accepted:  indented  "),
            Some("indented")
        );
        assert_eq!(acceptance("CPU-Increase-Accepted:\n"), None);
        assert_eq!(acceptance("CPU-Increase-Accepted:   \nmore"), None);
        assert_eq!(acceptance("see <!-- CPU-Increase-Accepted: x -->"), None);
        assert_eq!(acceptance("cpu-increase-accepted: wrong case"), None);
        assert_eq!(acceptance(""), None);
    }

    #[test]
    fn rejects_a_gate_that_is_not_a_positive_percentage() {
        for percent in ["0", "-1", "nan", "inf", "two", ""] {
            assert!(parse_percent(percent).is_err(), "{percent:?}");
        }
        assert_eq!(parse_percent("2"), Ok(2.0));
        assert_eq!(parse_percent("2.5"), Ok(2.5));
    }

    #[test]
    fn noise_from_an_unchanged_pull_request_passes() {
        // The largest four-thread deviation observed in a run whose serial
        // count did not move (#415, run 35991366630).
        let report = evaluate(
            &[both(66_744_131, 66_744_187), both(68_388_066, 68_012_566)],
            "2",
            true,
            None,
        )
        .unwrap();
        assert!(report.failure.is_none(), "{}", report.markdown);
        assert!(
            report
                .markdown
                .contains("| 1 thread | 66,744,187 | 66,744,131 | 1.0000x | within 2% |")
        );
    }

    #[test]
    fn an_increase_at_the_threshold_passes_and_one_above_it_fails() {
        let at = evaluate(&[both(102, 100), both(100, 100)], "2", true, None).unwrap();
        assert!(at.failure.is_none(), "{}", at.markdown);

        let above = evaluate(&[both(100, 100), both(1_021, 1_000)], "2", true, None).unwrap();
        let failure = above.failure.expect("four threads is over the gate");
        assert!(
            failure.contains("more than 2% over the merge base on: 4 threads."),
            "{failure}"
        );
        assert!(failure.contains(ACCEPTANCE_MARKER), "{failure}");
        assert!(above.markdown.contains("| 1.0210x | **over 2%** |"));
    }

    #[test]
    fn a_decrease_of_any_size_passes() {
        let report = evaluate(&[both(50, 100), both(1, 100)], "2", true, None).unwrap();
        assert!(report.failure.is_none());
    }

    #[test]
    fn an_accepted_increase_passes_and_says_why() {
        let report = evaluate(
            &[both(110, 100), both(110, 100)],
            "2",
            true,
            Some("a new correctness check per entry"),
        )
        .unwrap();
        assert!(report.failure.is_none(), "{}", report.markdown);
        assert!(report.markdown.contains("over 2%, accepted"));
        assert!(
            report
                .markdown
                .contains("a new correctness check per entry")
        );
    }

    #[test]
    fn a_merge_base_without_the_harness_is_reported_rather_than_gated() {
        let head_only = [
            Counts {
                head: Some(66_744_131),
                base: None,
            },
            Counts {
                head: Some(69_160_696),
                base: None,
            },
        ];
        let pull_request = evaluate(&head_only, "2", true, None).unwrap();
        assert!(pull_request.failure.is_none());
        assert!(pull_request.markdown.contains("| 4 threads | 69,160,696 |"));
        assert!(pull_request.markdown.contains("predates this lane"));

        let push = evaluate(&head_only, "2", false, None).unwrap();
        assert!(push.failure.is_none());
        assert!(!push.markdown.contains("predates this lane"));
    }

    #[test]
    fn a_missing_measurement_fails_instead_of_passing_silently() {
        let no_head = [
            Counts {
                head: None,
                base: Some(100),
            },
            both(100, 100),
        ];
        let failure = evaluate(&no_head, "2", true, None).unwrap().failure;
        assert!(failure.is_some_and(|failure| failure.contains("no head count for: 1 thread")));

        let half_base = [
            both(100, 100),
            Counts {
                head: Some(100),
                base: None,
            },
        ];
        let failure = evaluate(&half_base, "2", true, None).unwrap().failure;
        assert!(failure.is_some_and(|failure| {
            failure.contains("merge base produced no count for: 4 threads")
        }));
    }
}
