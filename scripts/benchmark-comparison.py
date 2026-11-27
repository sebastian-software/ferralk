from pathlib import Path
import sys

baseline, main = map(Path, sys.argv[1:3])
path = Path("crates/ferralk/src/ignore_rules.rs")
source = (baseline / path).read_text()
start = "    fn rule_engine_cost() {\n"
prefix, benchmark = source.split(start, 1)
current = (main / path).read_text().split(start, 1)[1]
setup = current.split("        use std::time::Instant;", 1)[0]
old, rest = benchmark.split("    }\n}", 1)
old = old.replace("RuleSetBuilder::new(&root, false, false)", "RuleSetBuilder::new(&root, ignore_case, ignore_case)")
old = old.replace(
    "            let mut theirs = ignore::gitignore::GitignoreBuilder::new(&root);",
    "            let mut theirs = ignore::gitignore::GitignoreBuilder::new(&root);\n"
    '            theirs.case_insensitive(ignore_case).expect("case option is valid");',
)
old = old.replace("let rounds = 200;", "let rounds = 5_000;")
old = old.replace("{count:>3} rules: ours", "{mode}, {count:>3} rules: ours")
assert (old.count("ignore_case"), old.count("5_000"), old.count("{mode}")) == (3, 1, 1)
(baseline / path).write_text(prefix + start + setup + old + "    }\n}" + rest)
