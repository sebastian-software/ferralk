#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{ErrorPolicy, Verdict, WalkOptions, Walker};
#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, PermissionsExt};

static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

struct Fixture {
    root: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let unique = format!(
            "ferralk-integration-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
                + NEXT_FIXTURE.fetch_add(1, Ordering::Relaxed) as u128
        );
        let root = std::env::temp_dir().join(unique);
        fs::create_dir_all(&root).expect("create fixture root");
        Self { root }
    }

    fn write(&self, path: impl AsRef<Path>) {
        let path = self.root.join(path);
        fs::create_dir_all(path.parent().expect("fixture file has parent"))
            .expect("create fixture parent");
        fs::write(path, b"fixture").expect("write fixture file");
    }

    fn directory(&self, path: impl AsRef<Path>) {
        fs::create_dir_all(self.root.join(path)).expect("create fixture directory");
    }

    /// A relative symlink, so the fixture stays movable and a broken link stays
    /// broken for the right reason.
    #[cfg(unix)]
    fn link(&self, target: impl AsRef<Path>, name: impl AsRef<Path>) {
        std::os::unix::fs::symlink(target.as_ref(), self.root.join(name))
            .expect("create fixture symlink");
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Runs one walker configuration through every frontend - serial `collect`,
/// parallel `collect`, `visit` on one and four threads, and `stream` - and
/// returns the sorted
/// root-relative paths after asserting that they agree. The frontends schedule
/// directories differently but classify entries in one place, so a filter that
/// reaches one of them has to reach all three.
fn paths_from_every_frontend(configure: impl Fn(Walker) -> Walker, root: &Path) -> Vec<PathBuf> {
    paths_from_every_frontend_with(WalkOptions::default(), configure, root)
}

/// The same agreement check for a configuration that needs [`WalkOptions`],
/// which every frontend has to receive alike - `stream` included, since it is
/// the one that does not take the sorted collect path.
fn paths_from_every_frontend_with(
    options: WalkOptions,
    configure: impl Fn(Walker) -> Walker,
    root: &Path,
) -> Vec<PathBuf> {
    let options = options.sort(true);
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .expect("entry is rooted in fixture")
            .to_path_buf()
    };
    let collected = |threads: usize| {
        configure(Walker::new(root))
            .threads(threads)
            .options(options)
            .collect()
            .expect("collect succeeds")
            .entries()
            .iter()
            .map(|entry| relative(entry.path()))
            .collect::<Vec<_>>()
    };
    let visited = |threads: usize| {
        configure(Walker::new(root))
            .threads(threads)
            .options(options)
            .visit(|_| Verdict::Keep)
            .expect("visit succeeds")
            .entries()
            .iter()
            .map(|entry| relative(entry.path()))
            .collect::<Vec<_>>()
    };
    let serial = collected(1);
    assert_eq!(collected(4), serial, "parallel collect differs from serial");
    // The visitor is a fourth frontend over the same pipeline, so keeping every
    // entry has to reproduce a plain collect on whatever this fixture holds.
    assert_eq!(
        visited(1),
        serial,
        "serial visit differs from serial collect"
    );
    assert_eq!(
        visited(4),
        serial,
        "parallel visit differs from serial collect"
    );
    let mut streamed = configure(Walker::new(root))
        .options(options)
        .stream()
        .map(|entry| relative(entry.expect("stream succeeds").path()))
        .collect::<Vec<_>>();
    streamed.sort();
    assert_eq!(streamed, serial, "stream differs from serial collect");
    serial
}

/// The shape that cost the Palamedes trial (sebastian-software/palamedes#878)
/// its parity: visible sources below a hidden directory. Their leading period
/// belongs to a directory component, so an ordinary wildcard refuses the whole
/// subtree until `match_hidden` says otherwise.
#[test]
fn match_hidden_admits_visible_files_below_hidden_directories() {
    let fixture = Fixture::new();
    fixture.write("src/app.ts");
    fixture.write("src/app.js");
    fixture.write("site/app.ts");
    fixture.write(".react-router/types.ts");
    fixture.write("site/.react-router/routes.ts");
    fixture.write(".hidden.ts");

    let default = paths_from_every_frontend(
        |walker| walker.include("**/*.ts").expect("valid include"),
        &fixture.root,
    );
    assert_eq!(
        default,
        vec![PathBuf::from("site/app.ts"), PathBuf::from("src/app.ts")]
    );

    let hidden = paths_from_every_frontend(
        |walker| {
            walker
                .match_hidden(true)
                .include("**/*.ts")
                .expect("valid include")
        },
        &fixture.root,
    );
    assert_eq!(
        hidden,
        vec![
            PathBuf::from(".hidden.ts"),
            PathBuf::from(".react-router/types.ts"),
            PathBuf::from("site/.react-router/routes.ts"),
            PathBuf::from("site/app.ts"),
            PathBuf::from("src/app.ts"),
        ]
    );

    // The option is matcher semantics, so a traversal filter still overrules
    // it: `skip_hidden` removes the same entries before any pattern runs.
    let skipped = Walker::new(&fixture.root)
        .match_hidden(true)
        .include("**/*.ts")
        .expect("valid include")
        .options(WalkOptions::default().sort(true).skip_hidden(true))
        .collect()
        .expect("collect succeeds")
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(skipped, default);
}

/// Two things the option must not break: the prune planner still descends
/// below a visible literal root when the next component is hidden, and an
/// exclude wildcard covers a leading period exactly like an include one.
#[test]
fn match_hidden_reaches_excludes_and_the_prune_planner_alike() {
    let fixture = Fixture::new();
    fixture.write("src/app.ts");
    fixture.write("site/app.ts");
    fixture.write("site/.react-router/routes.ts");
    fixture.write(".react-router/types.ts");
    fixture.write(".hidden.ts");

    let scoped = paths_from_every_frontend(
        |walker| {
            walker
                .match_hidden(true)
                .include("site/**/*.ts")
                .expect("valid include")
        },
        &fixture.root,
    );
    assert_eq!(
        scoped,
        vec![
            PathBuf::from("site/.react-router/routes.ts"),
            PathBuf::from("site/app.ts"),
        ]
    );

    // `*/*.ts` reaches `.react-router/types.ts` only when the exclude side of
    // the walker was compiled with the option too.
    let excluded = paths_from_every_frontend(
        |walker| {
            walker
                .match_hidden(true)
                .include("**/*.ts")
                .expect("valid include")
                .exclude("*/*.ts")
                .expect("valid exclude")
        },
        &fixture.root,
    );
    assert_eq!(
        excluded,
        vec![
            PathBuf::from(".hidden.ts"),
            PathBuf::from("site/.react-router/routes.ts"),
        ]
    );
}

/// An extglob behind a component-local star used to lose the recursive
/// prefix's backtrack point, so `**/*.@(ts|js)` stopped one directory deep
/// while `**/*.ts` did not (issue #393).
#[test]
fn recursive_include_with_an_extglob_suffix_reaches_every_depth() {
    let fixture = Fixture::new();
    fixture.write("a.ts");
    fixture.write("src/b.js");
    fixture.write("src/lib/c.ts");
    fixture.write("src/lib/deep/d.js");
    fixture.write("src/lib/deep/e.rs");

    let selected = paths_from_every_frontend(
        |walker| walker.include("**/*.@(ts|js)").expect("valid include"),
        &fixture.root,
    );
    assert_eq!(
        selected,
        vec![
            PathBuf::from("a.ts"),
            PathBuf::from("src/b.js"),
            PathBuf::from("src/lib/c.ts"),
            PathBuf::from("src/lib/deep/d.js"),
        ]
    );
}

#[test]
fn collect_retains_a_disappearing_root_error() {
    let fixture = Fixture::new();
    fs::remove_dir_all(&fixture.root).expect("remove fixture root before walking");

    let result = Walker::new(&fixture.root)
        .error_policy(ErrorPolicy::Collect)
        .collect()
        .expect("collect returns partial result");
    assert_eq!(result.errors().len(), 1);
    assert_eq!(result.errors()[0].operation().as_str(), "read_dir");
}

#[test]
fn stream_yields_a_disappearing_root_error() {
    let fixture = Fixture::new();
    fs::remove_dir_all(&fixture.root).expect("remove fixture root before walking");

    let mut stream = Walker::new(&fixture.root)
        .error_policy(ErrorPolicy::Collect)
        .stream();
    let error = stream
        .next()
        .expect("stream yields the error")
        .expect_err("missing root is an error event");
    assert_eq!(error.operation().as_str(), "read_dir");
    assert!(stream.next().is_none());
}

#[test]
fn skip_hidden_excludes_hidden_files_and_subtrees_across_collect_and_stream() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("visible.txt"), b"fixture").expect("write visible file");
    fs::write(fixture.root.join(".hidden.txt"), b"fixture").expect("write hidden file");
    fs::create_dir_all(fixture.root.join(".cache")).expect("create hidden fixture directory");
    fs::write(fixture.root.join(".cache/inside.txt"), b"fixture")
        .expect("write hidden subtree file");
    fs::create_dir_all(fixture.root.join("nested")).expect("create visible fixture directory");
    fs::write(fixture.root.join("nested/visible.txt"), b"fixture")
        .expect("write nested visible file");

    let options = WalkOptions::default().skip_hidden(true).sort(true);
    let collected = Walker::new(&fixture.root)
        .threads(1)
        .options(options)
        .collect()
        .expect("collect succeeds");
    let collected_relative = collected
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        collected_relative,
        vec![
            PathBuf::from("nested"),
            PathBuf::from("nested/visible.txt"),
            PathBuf::from("visible.txt"),
        ]
    );

    let mut streamed_relative = Walker::new(&fixture.root)
        .options(options)
        .stream()
        .map(|entry| {
            entry
                .expect("stream succeeds")
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    streamed_relative.sort();
    assert_eq!(streamed_relative, collected_relative);

    let parallel_relative = Walker::new(&fixture.root)
        .threads(4)
        .options(options)
        .collect()
        .expect("parallel collect succeeds")
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(parallel_relative, collected_relative);
}

#[test]
fn files_only_excludes_directories_without_pruning_their_contents() {
    let fixture = Fixture::new();
    fs::write(fixture.root.join("root.txt"), b"fixture").expect("write root file");
    fs::create_dir_all(fixture.root.join("nested")).expect("create nested fixture directory");
    fs::write(fixture.root.join("nested/child.txt"), b"fixture").expect("write nested file");

    let options = WalkOptions::default().files_only(true).sort(true);
    let collected = Walker::new(&fixture.root)
        .threads(1)
        .options(options)
        .collect()
        .expect("collect succeeds");
    let collected_relative = collected
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        collected_relative,
        vec![PathBuf::from("nested/child.txt"), PathBuf::from("root.txt")]
    );

    let mut streamed_relative = Walker::new(&fixture.root)
        .options(options)
        .stream()
        .map(|entry| {
            entry
                .expect("stream succeeds")
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    streamed_relative.sort();
    assert_eq!(streamed_relative, collected_relative);

    let parallel_relative = Walker::new(&fixture.root)
        .threads(4)
        .options(options)
        .collect()
        .expect("parallel collect succeeds")
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(parallel_relative, collected_relative);
}

#[cfg(unix)]
#[test]
fn a_brace_scoped_include_prunes_an_unreadable_sibling_before_opening_it() {
    // The braced form of the include above. Its literal roots only survive the
    // planner if the brace alternatives are expanded, so without that the walk
    // opens `locked/` and aborts.
    let fixture = Fixture::new();
    if fs::metadata(&fixture.root)
        .expect("fixture root metadata")
        .uid()
        == 0
    {
        return;
    }
    fs::create_dir_all(fixture.root.join("src")).expect("create source directory");
    fs::write(fixture.root.join("src/a.rs"), b"fixture").expect("write source fixture");
    fs::create_dir_all(fixture.root.join("lib")).expect("create library directory");
    fs::write(fixture.root.join("lib/b.rs"), b"fixture").expect("write library fixture");
    fs::create_dir_all(fixture.root.join("locked")).expect("create locked directory");
    fs::write(fixture.root.join("locked/secret.rs"), b"fixture").expect("write locked fixture");
    let locked = fixture.root.join("locked");
    let original_permissions = fs::metadata(&locked)
        .expect("locked directory metadata")
        .permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o0))
        .expect("make locked directory unreadable");

    let result = Walker::new(&fixture.root)
        .include("{src,lib}/**/*.rs")
        .expect("valid braced include")
        .error_policy(ErrorPolicy::Abort)
        .options(WalkOptions::default().sort(true))
        .collect();

    fs::set_permissions(&locked, original_permissions).expect("restore locked permissions");
    let result = result.expect("out-of-scope unreadable directory is pruned");
    let paths = result
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry belongs to fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        paths,
        vec![PathBuf::from("lib/b.rs"), PathBuf::from("src/a.rs")]
    );
}

#[cfg(unix)]
#[test]
fn scoped_include_prunes_an_unreadable_sibling_before_opening_it() {
    // Ported from zlob/test/test_walk.zig's out-of-scope unreadable-directory
    // regression. Root can bypass mode bits, so that environment cannot prove
    // the intended access boundary.
    let fixture = Fixture::new();
    if fs::metadata(&fixture.root)
        .expect("fixture root metadata")
        .uid()
        == 0
    {
        return;
    }
    fs::create_dir_all(fixture.root.join("src")).expect("create source directory");
    fs::write(fixture.root.join("src/a.rs"), b"fixture").expect("write source fixture");
    fs::create_dir_all(fixture.root.join("locked")).expect("create locked directory");
    fs::write(fixture.root.join("locked/secret.rs"), b"fixture").expect("write locked fixture");
    let locked = fixture.root.join("locked");
    let original_permissions = fs::metadata(&locked)
        .expect("locked directory metadata")
        .permissions();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o0))
        .expect("make locked directory unreadable");

    let result = Walker::new(&fixture.root)
        .include("src/**/*.rs")
        .expect("valid scoped include")
        .error_policy(ErrorPolicy::Abort)
        .options(WalkOptions::default().sort(true))
        .collect();

    fs::set_permissions(&locked, original_permissions).expect("restore locked permissions");
    let result = result.expect("out-of-scope unreadable directory is pruned");
    let paths = result
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry belongs to fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(paths, vec![PathBuf::from("src/a.rs")]);
}

#[test]
fn gitignore_hides_dot_git_unless_keep_git_dir_is_enabled() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.root.join(".git")).expect("create git fixture directory");
    fs::write(fixture.root.join(".git/config"), b"fixture").expect("write git config fixture");
    fs::write(fixture.root.join("visible.txt"), b"fixture").expect("write visible fixture");

    let hidden = Walker::new(&fixture.root)
        .threads(1)
        .respect_git_ignore(true)
        .options(WalkOptions::default().sort(true))
        .collect()
        .expect("collect succeeds");
    let hidden_relative = hidden
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_relative, vec![PathBuf::from("visible.txt")]);

    let hidden_parallel = Walker::new(&fixture.root)
        .threads(4)
        .respect_git_ignore(true)
        .options(WalkOptions::default().sort(true))
        .collect()
        .expect("parallel collect succeeds");
    let hidden_parallel_relative = hidden_parallel
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(hidden_parallel_relative, hidden_relative);

    let mut hidden_streamed_relative = Walker::new(&fixture.root)
        .respect_git_ignore(true)
        .options(WalkOptions::default().sort(true))
        .stream()
        .map(|entry| {
            entry
                .expect("stream succeeds")
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    hidden_streamed_relative.sort();
    assert_eq!(hidden_streamed_relative, hidden_relative);

    let options = WalkOptions::default().keep_git_dir(true).sort(true);
    let kept = Walker::new(&fixture.root)
        .threads(4)
        .respect_git_ignore(true)
        .options(options)
        .collect()
        .expect("parallel collect succeeds");
    let kept_relative = kept
        .entries()
        .iter()
        .map(|entry| {
            entry
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    assert_eq!(
        kept_relative,
        vec![
            PathBuf::from(".git"),
            PathBuf::from(".git/config"),
            PathBuf::from("visible.txt"),
        ]
    );

    let mut streamed_relative = Walker::new(&fixture.root)
        .respect_git_ignore(true)
        .options(options)
        .stream()
        .map(|entry| {
            entry
                .expect("stream succeeds")
                .path()
                .strip_prefix(&fixture.root)
                .expect("entry is rooted in fixture")
                .to_path_buf()
        })
        .collect::<Vec<_>>();
    streamed_relative.sort();
    assert_eq!(streamed_relative, kept_relative);
}

#[cfg(unix)]
#[test]
fn collect_retains_an_unreadable_directory_error() {
    let fixture = Fixture::new();
    let locked = fixture.root.join("locked");
    fs::create_dir(&locked).expect("create locked fixture directory");
    let original_permissions = fs::metadata(&locked)
        .expect("read fixture metadata")
        .permissions();
    let mut unreadable_permissions = original_permissions.clone();
    unreadable_permissions.set_mode(0o000);
    fs::set_permissions(&locked, unreadable_permissions).expect("make fixture unreadable");

    // Root has CAP_DAC_OVERRIDE, so chmod 000 does not make this fixture
    // unreadable in containers or privileged development environments.
    if fs::read_dir(&locked).is_ok() {
        fs::set_permissions(&locked, original_permissions).expect("restore fixture permissions");
        return;
    }

    let result = Walker::new(&fixture.root)
        .threads(1)
        .error_policy(ErrorPolicy::Collect)
        .collect();
    fs::set_permissions(&locked, original_permissions).expect("restore fixture permissions");
    let result = result.expect("collect returns partial result");

    assert!(
        result
            .errors()
            .iter()
            .any(|error| error.operation().as_str() == "read_dir" && error.path() == locked)
    );
}

#[cfg(target_os = "linux")]
#[test]
fn preserves_non_utf8_path_in_public_integration_api() {
    use std::os::unix::ffi::OsStringExt;

    let fixture = Fixture::new();
    let name = std::ffi::OsString::from_vec(vec![b'n', 0xFF]);
    fixture.write(PathBuf::from(&name));

    let result = Walker::new(&fixture.root)
        .options(WalkOptions::default().sort(true))
        .collect()
        .expect("walk succeeds");
    assert!(result.entries().iter().any(|entry| {
        entry
            .path()
            .strip_prefix(&fixture.root)
            .is_ok_and(|relative| relative == Path::new(&name))
    }));
}

/// One tree holding an ordinary file, an ordinary directory, and one link of
/// each of the three shapes.
#[cfg(unix)]
fn symlink_fixture() -> Fixture {
    let fixture = Fixture::new();
    fixture.write("plain.txt");
    fixture.directory("plain-dir");
    fixture.write("target-file.txt");
    fixture.directory("target-dir");
    fixture.link("target-file.txt", "link-to-file");
    fixture.link("target-dir", "link-to-dir");
    fixture.link("nowhere", "link-broken");
    fixture
}

/// The three symlink shapes against both kind filters, on every frontend.
///
/// A listing reports a symlink as a symlink and says nothing about its target,
/// so `files_only` counts every one of them as a file and `directories_only`
/// counts none of them as a directory. That is also what zlob 1.6.3 does under
/// `ZLOB_WALK_NO_REPORT_DIRS` - measured by the oracle in
/// `tools/oracle/tests/zlob_walk_symlinks.rs` - so it stays the default.
#[cfg(unix)]
#[test]
fn symlink_kinds_reach_both_filters_unresolved_by_default() {
    let fixture = symlink_fixture();

    let files = paths_from_every_frontend_with(
        WalkOptions::default().files_only(true),
        |walker| walker,
        &fixture.root,
    );
    assert_eq!(
        files,
        vec![
            PathBuf::from("link-broken"),
            PathBuf::from("link-to-dir"),
            PathBuf::from("link-to-file"),
            PathBuf::from("plain.txt"),
            PathBuf::from("target-file.txt"),
        ],
        "unresolved, every symlink counts as a file"
    );

    let directories = paths_from_every_frontend_with(
        WalkOptions::default().directories_only(true),
        |walker| walker,
        &fixture.root,
    );
    assert_eq!(
        directories,
        vec![PathBuf::from("plain-dir"), PathBuf::from("target-dir")],
        "unresolved, no symlink counts as a directory"
    );
}

/// The same three shapes with `resolve_symlink_kind`, which is what makes the
/// two filters agree with `Path::is_file` and `Path::is_dir`.
#[cfg(unix)]
#[test]
fn resolving_symlink_kind_sorts_the_three_shapes_by_target() {
    let fixture = symlink_fixture();

    let files = paths_from_every_frontend_with(
        WalkOptions::default()
            .files_only(true)
            .resolve_symlink_kind(true),
        |walker| walker,
        &fixture.root,
    );
    assert_eq!(
        files,
        vec![
            PathBuf::from("link-to-file"),
            PathBuf::from("plain.txt"),
            PathBuf::from("target-file.txt"),
        ],
        "resolved, only the link to a file counts as a file"
    );
    // The property the option exists for, checked against the standard library
    // rather than against itself.
    for entry in &files {
        assert!(
            fixture.root.join(entry).is_file(),
            "{} is not what Path::is_file calls a file",
            entry.display()
        );
    }

    let directories = paths_from_every_frontend_with(
        WalkOptions::default()
            .directories_only(true)
            .resolve_symlink_kind(true),
        |walker| walker,
        &fixture.root,
    );
    assert_eq!(
        directories,
        vec![
            PathBuf::from("link-to-dir"),
            PathBuf::from("plain-dir"),
            PathBuf::from("target-dir"),
        ],
        "resolved, the link to a directory counts as a directory"
    );
    for entry in &directories {
        assert!(
            fixture.root.join(entry).is_dir(),
            "{} is not what Path::is_dir calls a directory",
            entry.display()
        );
    }
}

/// A broken link is dropped rather than reported: dangling links are ordinary,
/// and an error for each would flood `errors()` and end an `Abort` walk over a
/// stale build artifact.
#[cfg(unix)]
#[test]
fn a_broken_link_is_dropped_without_an_error_under_every_policy() {
    let fixture = Fixture::new();
    fixture.write("plain.txt");
    fixture.link("nowhere", "link-broken");

    for policy in [ErrorPolicy::Collect, ErrorPolicy::Skip, ErrorPolicy::Abort] {
        for threads in [1, 4] {
            let result = Walker::new(&fixture.root)
                .threads(threads)
                .error_policy(policy)
                .options(
                    WalkOptions::default()
                        .files_only(true)
                        .resolve_symlink_kind(true)
                        .sort(true),
                )
                .collect()
                .unwrap_or_else(|error| {
                    panic!("{policy:?} on {threads} threads aborted on a broken link: {error}")
                });
            assert!(
                result.errors().is_empty(),
                "{policy:?} on {threads} threads reported an error for a broken link"
            );
            let names = result
                .entries()
                .iter()
                .map(|entry| {
                    entry
                        .path()
                        .strip_prefix(&fixture.root)
                        .expect("entry is rooted in fixture")
                        .to_path_buf()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                names,
                vec![PathBuf::from("plain.txt")],
                "{policy:?} on {threads} threads"
            );
        }
    }
}

/// Resolving does not change what the entry says it is: the link is still a
/// symlink, and only the kind filters looked past it.
#[cfg(unix)]
#[test]
fn resolving_leaves_the_reported_kind_alone() {
    let fixture = symlink_fixture();

    let result = Walker::new(&fixture.root)
        .options(
            WalkOptions::default()
                .files_only(true)
                .resolve_symlink_kind(true)
                .sort(true),
        )
        .collect()
        .expect("walk succeeds");
    let link = result
        .entries()
        .iter()
        .find(|entry| entry.path().ends_with("link-to-file"))
        .expect("the link to a file survives files_only");
    assert_eq!(link.kind(), ferralk::WalkEntryKind::Symlink);
    assert!(link.is_symlink());
    assert!(!link.is_dir());
}

/// Following symlinks already answers the kind question, so the option adds
/// nothing there - and must not subtract anything either.
///
/// The fixture leaves the broken link out on purpose: under `follow_symlinks`
/// that is a reported error rather than an entry, which is the asymmetry the
/// next test pins.
#[cfg(unix)]
#[test]
fn following_symlinks_classifies_the_same_way_with_or_without_resolving() {
    let fixture = Fixture::new();
    fixture.write("plain.txt");
    fixture.directory("plain-dir");
    fixture.write("target-file.txt");
    fixture.directory("target-dir");
    fixture.link("target-file.txt", "link-to-file");
    fixture.link("target-dir", "link-to-dir");

    for filters in [
        WalkOptions::default().files_only(true),
        WalkOptions::default().directories_only(true),
    ] {
        let following = filters.follow_symlinks(true);
        assert_eq!(
            paths_from_every_frontend_with(following, |walker| walker, &fixture.root),
            paths_from_every_frontend_with(
                following.resolve_symlink_kind(true),
                |walker| walker,
                &fixture.root
            ),
            "resolving changed a walk that already follows symlinks"
        );
    }
}

/// The two switches answer different questions about a broken link, and the
/// difference is deliberate.
///
/// `follow_symlinks` was told to walk *through* the link and cannot, so the
/// failed `metadata` is reported. `resolve_symlink_kind` only asked *what* the
/// link is, and "nothing is there" answers that: the entry is neither a file
/// nor a directory, so both filters drop it and no error is raised.
#[cfg(unix)]
#[test]
fn a_broken_link_errors_when_followed_and_is_merely_dropped_when_resolved() {
    let fixture = Fixture::new();
    fixture.write("plain.txt");
    fixture.link("nowhere", "link-broken");

    let errors_of = |options: WalkOptions| {
        Walker::new(&fixture.root)
            .error_policy(ErrorPolicy::Collect)
            .options(options.files_only(true).sort(true))
            .collect()
            .expect("collect retains errors rather than aborting")
            .errors()
            .iter()
            .map(|error| error.operation().as_str())
            .collect::<Vec<_>>()
    };

    assert_eq!(
        errors_of(WalkOptions::default().follow_symlinks(true)),
        vec!["metadata"],
        "following a broken link is a traversal failure and is reported"
    );
    assert!(
        errors_of(WalkOptions::default().resolve_symlink_kind(true)).is_empty(),
        "resolving a broken link is an answer, not a failure"
    );
}

/// A symlink outside `includes` — even a symlink loop that would error on
/// `stat(2)` — must not be statted by `resolve_symlink_kind`, and contradictory
/// `files_only && directories_only` must never stat any symlink either.
#[cfg(unix)]
#[test]
fn resolving_symlink_kind_skips_non_included_loops_and_contradictory_filters() {
    let fixture = Fixture::new();
    fixture.write("src/lib.rs");
    fixture.directory("src/sub");
    fixture.link("loop-b", "loop-a");
    fixture.link("loop-a", "loop-b");

    for options in [
        WalkOptions::default()
            .files_only(true)
            .resolve_symlink_kind(true)
            .sort(true),
        WalkOptions::default()
            .directories_only(true)
            .resolve_symlink_kind(true)
            .sort(true),
    ] {
        let expected = if options
            == WalkOptions::default()
                .files_only(true)
                .resolve_symlink_kind(true)
                .sort(true)
        {
            vec![PathBuf::from("src/lib.rs")]
        } else {
            vec![PathBuf::from("src"), PathBuf::from("src/sub")]
        };
        let actual = paths_from_every_frontend_with(
            options,
            |walker| {
                walker
                    .include("src/**")
                    .expect("valid include")
                    .error_policy(ErrorPolicy::Abort)
            },
            &fixture.root,
        );
        assert_eq!(actual, expected);
    }

    let empty = paths_from_every_frontend_with(
        WalkOptions::default()
            .files_only(true)
            .directories_only(true)
            .resolve_symlink_kind(true)
            .sort(true),
        |walker| walker.error_policy(ErrorPolicy::Abort),
        &fixture.root,
    );
    assert!(empty.is_empty());
}

/// A symlink whose target traverses a regular file fails `stat(2)` with
/// `ENOTDIR` (`ErrorKind::NotADirectory`): like `ENOENT`, nothing exists at the
/// target, so `resolve_symlink_kind` drops it without reporting an error.
#[cfg(unix)]
#[test]
fn resolving_symlink_kind_drops_not_a_directory_broken_link_without_error() {
    let fixture = Fixture::new();
    fixture.write("plain.txt");
    fixture.link("plain.txt/child", "link-through-file");

    let files = paths_from_every_frontend_with(
        WalkOptions::default()
            .files_only(true)
            .resolve_symlink_kind(true),
        |walker| walker.error_policy(ErrorPolicy::Abort),
        &fixture.root,
    );
    assert_eq!(files, vec![PathBuf::from("plain.txt")]);

    let dirs = paths_from_every_frontend_with(
        WalkOptions::default()
            .directories_only(true)
            .resolve_symlink_kind(true),
        |walker| walker.error_policy(ErrorPolicy::Abort),
        &fixture.root,
    );
    assert!(dirs.is_empty());
}

/// A pre-cancelled walk under `ErrorPolicy::Abort` reports cancellation with an
/// empty, error-free result across every public frontend (`collect`, `visit`,
/// `stream`, `stream_parallel`), even when an ancestor `.gitignore` is
/// unreadable. Zero-I/O guarantees are pinned separately by the counting-backend
/// and lazy-stream unit tests in `lib.rs`.
#[test]
fn pre_cancelled_walks_report_cancelled_and_empty_across_every_frontend() {
    let fixture = Fixture::new();
    fixture.directory(".git");
    // A directory named `.gitignore` causes `read_ignore_file` to fail when
    // `IgnoreScope::for_root` inspects ancestor directories.
    fixture.directory(".gitignore");
    fixture.write("nested/file.txt");
    let nested = fixture.root.join("nested");

    // Sanity check: an un-cancelled walk under `ErrorPolicy::Abort` fails while
    // reading the ancestor `.gitignore`.
    let error = Walker::new(&nested)
        .respect_git_ignore(true)
        .error_policy(ErrorPolicy::Abort)
        .collect()
        .expect_err("uncancelled walk must report the unreadable ancestor .gitignore");
    assert_eq!(error.operation().as_str(), "read_ignore");

    let token = ferralk::CancellationToken::default();
    token.cancel();

    for threads in [1, 4] {
        let collected = Walker::new(&nested)
            .threads(threads)
            .respect_git_ignore(true)
            .error_policy(ErrorPolicy::Abort)
            .cancellation(token.clone())
            .collect()
            .expect("pre-cancelled collect must not read ancestor .gitignore");
        assert!(collected.was_cancelled());
        assert!(collected.entries().is_empty());
        assert!(collected.errors().is_empty());

        let visited = Walker::new(&nested)
            .threads(threads)
            .respect_git_ignore(true)
            .error_policy(ErrorPolicy::Abort)
            .cancellation(token.clone())
            .visit(|_| ferralk::Verdict::Keep)
            .expect("pre-cancelled visit must not read ancestor .gitignore");
        assert!(visited.was_cancelled());
        assert!(visited.entries().is_empty());
        assert!(visited.errors().is_empty());

        let mut parallel_stream = Walker::new(&nested)
            .threads(threads)
            .respect_git_ignore(true)
            .error_policy(ErrorPolicy::Abort)
            .cancellation(token.clone())
            .stream_parallel();
        assert!(parallel_stream.next().is_none());
        assert!(parallel_stream.was_cancelled());

        let deferred = ferralk::CancellationToken::default();
        let mut parallel_stream = Walker::new(&nested)
            .threads(threads)
            .respect_git_ignore(true)
            .error_policy(ErrorPolicy::Abort)
            .cancellation(deferred.clone())
            .stream_parallel();
        deferred.cancel();
        assert!(parallel_stream.next().is_none());
        assert!(parallel_stream.was_cancelled());
    }

    let mut stream = Walker::new(&nested)
        .respect_git_ignore(true)
        .error_policy(ErrorPolicy::Abort)
        .cancellation(token.clone())
        .stream();
    assert!(stream.next().is_none());
    assert!(stream.was_cancelled());

    let deferred = ferralk::CancellationToken::default();
    let mut stream = Walker::new(&nested)
        .respect_git_ignore(true)
        .error_policy(ErrorPolicy::Abort)
        .cancellation(deferred.clone())
        .stream();
    deferred.cancel();
    assert!(stream.next().is_none());
    assert!(stream.was_cancelled());
}

/// The fixture path in the spelling an absolute pattern is written in: `/`
/// separators on every platform, per ADR-0005.
fn pattern_spelling(path: &Path) -> String {
    path.to_str()
        .expect("the temporary directory is UTF-8 on a test host")
        .replace('\\', "/")
}

/// A walk root with a `..` component is rejected for every absolute pattern,
/// through every builder entry point, wherever the pattern stops agreeing with
/// the root's spelling (#396). Before, `$B/a/**` got the error while `$B/b/**`
/// and `$B/zzz/**` returned an empty walk.
#[test]
fn a_walk_root_with_dot_dot_rejects_every_absolute_pattern() {
    const ROOT_DOT_DOT: &str = "an absolute pattern needs a walk root without a `..` component";
    let fixture = Fixture::new();
    fixture.directory("a");
    fixture.write("b/y.txt");
    let dotted_root = fixture.root.join("a").join("..").join("b");
    let base = pattern_spelling(&fixture.root);
    let patterns = [
        format!("{base}/a/**"),
        format!("{base}/b/**"),
        format!("{base}/zzz/**"),
    ];

    for pattern in &patterns {
        for add in [Walker::include, Walker::exclude] {
            let error = add(Walker::new(&dotted_root), pattern.as_str())
                .err()
                .unwrap_or_else(|| panic!("{pattern} was accepted under a root with `..`"));
            assert_eq!(error.message(), ROOT_DOT_DOT, "{pattern}");
        }

        // Adding the root after the pattern reports the same rejection, and
        // the borrowed form leaves the walker's one root in place.
        let later = Walker::new(fixture.root.join("b"))
            .include(pattern.as_str())
            .expect("an absolute pattern under a plain root is accepted")
            .add_root(&dotted_root)
            .err()
            .unwrap_or_else(|| panic!("{pattern} was accepted for an added root with `..`"));
        assert_eq!(later.message(), ROOT_DOT_DOT, "{pattern}");

        let mut walker = Walker::new(fixture.root.join("b"))
            .exclude(pattern.as_str())
            .expect("an absolute pattern under a plain root is accepted");
        let error = walker
            .try_add_root(&dotted_root)
            .err()
            .unwrap_or_else(|| panic!("{pattern} was accepted for an added root with `..`"));
        assert_eq!(error.message(), ROOT_DOT_DOT, "{pattern}");
        assert_eq!(walker.roots().len(), 1, "the rejected root is not added");
    }

    // A relative pattern needs no arithmetic against the root, so the same
    // root still walks.
    assert_eq!(
        paths_from_every_frontend(
            |walker| walker.include("**/*.txt").expect("valid include"),
            &dotted_root,
        ),
        vec![PathBuf::from("y.txt")]
    );
}

/// A leading `!` is list negation in fast-glob, globby and `.gitignore`, and
/// the walker has none, so the ported pattern used to compile, ask for a
/// directory literally named `!src`, and select nothing (#397). It is now
/// refused through every builder entry point, while the spellings that mean
/// something here keep their meaning.
#[test]
fn a_leading_bang_is_refused_instead_of_selecting_nothing() {
    const LEADING: &str = "a leading `!` does not negate a walker pattern; pass the pattern without it to `exclude()` instead, or write `\\!` for a literal `!`";
    const ALTERNATIVE: &str = "a brace alternative starting with `!` does not negate a walker pattern; pass that alternative without it to `exclude()` instead, or write `\\!` for a literal `!`";
    let fixture = Fixture::new();
    fixture.write("src/a.ts");
    fixture.write("!src/b.ts");
    fixture.write("lib/c.ts");

    for (pattern, offset, message) in [
        ("!src/**", 0, LEADING),
        ("!*.ts", 0, LEADING),
        ("!", 0, LEADING),
        ("{!src,lib}/**", 1, ALTERNATIVE),
    ] {
        for add in [Walker::include, Walker::exclude] {
            let error = add(Walker::new(&fixture.root), pattern)
                .err()
                .unwrap_or_else(|| panic!("{pattern} was accepted"));
            assert_eq!(
                (error.offset(), error.message()),
                (offset, message),
                "{pattern}"
            );
        }

        // The borrowed forms leave the configured walker as it was.
        let mut walker = Walker::new(&fixture.root);
        walker.try_include("lib/**").expect("valid include");
        assert!(walker.try_include(pattern).is_err(), "{pattern}");
        assert!(walker.try_exclude(pattern).is_err(), "{pattern}");
        let files = walker
            .options(WalkOptions::default().files_only(true).sort(true))
            .collect()
            .expect("collect succeeds")
            .entries()
            .iter()
            .map(|entry| {
                entry
                    .path()
                    .strip_prefix(&fixture.root)
                    .unwrap()
                    .to_path_buf()
            })
            .collect::<Vec<_>>();
        assert_eq!(files, [Path::new("lib/c.ts")], "{pattern}");
    }

    let files = |configure: &dyn Fn(Walker) -> Walker| {
        paths_from_every_frontend_with(
            WalkOptions::default().files_only(true),
            configure,
            &fixture.root,
        )
    };
    // The escape spells the literal directory, as it always did.
    assert_eq!(
        files(&|walker| walker.include(r"\!src/**").expect("escaped `!`")),
        [PathBuf::from("!src/b.ts")]
    );
    // `!(` is the negated extglob, which is a pattern and not list negation.
    assert_eq!(
        files(&|walker| walker.include("!(src)/**").expect("negated extglob")),
        [PathBuf::from("!src/b.ts"), PathBuf::from("lib/c.ts")]
    );
    // What the ported list meant, written as the walker spells it.
    assert_eq!(
        files(&|walker| walker.exclude("src/**").expect("plain exclude")),
        [PathBuf::from("!src/b.ts"), PathBuf::from("lib/c.ts")]
    );
    // Below an absolute pattern's root the `!` starts no pattern anyone
    // wrote, so it stays a literal component.
    let absolute = format!("{}/!src/**", pattern_spelling(&fixture.root));
    assert_eq!(
        files(&|walker| walker.include(absolute.as_str()).expect("absolute pattern")),
        [PathBuf::from("!src/b.ts")]
    );
}

#[test]
fn same_file_system_keeps_same_volume_entries_across_frontends() {
    let fixture = Fixture::new();
    fixture.write("nested/file.txt");
    let expected = paths_from_every_frontend_with(
        WalkOptions::default().same_file_system(true),
        |walker| walker,
        &fixture.root,
    );
    assert_eq!(
        expected,
        vec![PathBuf::from("nested"), PathBuf::from("nested/file.txt")]
    );
    let mut parallel: Vec<_> = Walker::new(&fixture.root)
        .options(WalkOptions::default().same_file_system(true))
        .stream_parallel()
        .map(|entry| {
            entry
                .expect("parallel stream succeeds")
                .path()
                .strip_prefix(&fixture.root)
                .unwrap()
                .to_path_buf()
        })
        .collect();
    parallel.sort();
    assert_eq!(parallel, expected);
}

#[cfg(unix)]
#[test]
fn same_file_system_prunes_a_followed_link_to_another_device() {
    use std::os::unix::fs::MetadataExt;
    let fixture = Fixture::new();
    let other = Path::new("/dev");
    let Ok(metadata) = fs::metadata(other) else {
        return;
    };
    if metadata.dev() == fs::metadata(&fixture.root).unwrap().dev() {
        return;
    }
    fixture.write("local/file");
    fixture.link(other, "other");
    let followed = WalkOptions::default()
        .same_file_system(true)
        .follow_symlinks(true);
    let expected = paths_from_every_frontend_with(followed, |walker| walker, &fixture.root);
    assert!(expected.contains(&PathBuf::from("other")));
    assert!(
        !expected
            .iter()
            .any(|path| path != Path::new("other") && path.starts_with("other"))
    );
    assert!(expected.contains(&PathBuf::from("local/file")));
    let mut parallel: Vec<_> = Walker::new(&fixture.root)
        .options(followed)
        .stream_parallel()
        .map(|entry| {
            entry
                .expect("parallel stream succeeds")
                .path()
                .strip_prefix(&fixture.root)
                .unwrap()
                .to_path_buf()
        })
        .collect();
    parallel.sort();
    assert_eq!(parallel, expected);
    let not_followed = paths_from_every_frontend_with(
        WalkOptions::default().same_file_system(true),
        |walker| walker,
        &fixture.root,
    );
    assert!(not_followed.contains(&PathBuf::from("other")));
    assert!(
        !not_followed
            .iter()
            .any(|path| path != Path::new("other") && path.starts_with("other"))
    );
    // An explicit symlink root is opened even without follow_symlinks.
    let root_alias = Walker::new(fixture.root.join("other"))
        .options(WalkOptions::default().same_file_system(true).max_depth(1))
        .collect()
        .unwrap();
    assert!(root_alias.errors().is_empty());
    assert!(!root_alias.entries().is_empty());
}

#[test]
fn same_file_system_reports_missing_root_in_all_frontends() {
    use ferralk::{ErrorPolicy, WalkOperation};
    let fixture = Fixture::new();
    let root = fixture.root.join("missing");
    for policy in [ErrorPolicy::Collect, ErrorPolicy::Skip, ErrorPolicy::Abort] {
        let walker = || {
            Walker::new(&root)
                .error_policy(policy)
                .options(WalkOptions::default().same_file_system(true))
        };
        let collected = walker().collect();
        let check = |error: &ferralk::WalkError| {
            assert_eq!(error.operation(), WalkOperation::Metadata);
            assert_eq!(error.path(), root);
        };
        if policy == ErrorPolicy::Abort {
            check(&collected.unwrap_err());
        } else {
            let result = collected.unwrap();
            assert_eq!(result.errors().len(), 1);
            check(&result.errors()[0]);
        }
        for error in [
            walker().stream().next().unwrap().unwrap_err(),
            walker().stream_parallel().next().unwrap().unwrap_err(),
        ] {
            assert_eq!(error.operation(), WalkOperation::Metadata);
            assert_eq!(error.path(), root);
        }
    }
}
