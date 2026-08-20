#![forbid(unsafe_code)]

use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

use ferralk::{ErrorPolicy, WalkOptions, Walker};
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
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

/// Runs one walker configuration through all three frontends - serial
/// `collect`, parallel `collect`, and `stream` - and returns the sorted
/// root-relative paths after asserting that they agree. The frontends schedule
/// directories differently but classify entries in one place, so a filter that
/// reaches one of them has to reach all three.
fn paths_from_every_frontend(configure: impl Fn(Walker) -> Walker, root: &Path) -> Vec<PathBuf> {
    let relative = |path: &Path| {
        path.strip_prefix(root)
            .expect("entry is rooted in fixture")
            .to_path_buf()
    };
    let collected = |threads: usize| {
        configure(Walker::new(root))
            .threads(threads)
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("collect succeeds")
            .entries()
            .iter()
            .map(|entry| relative(entry.path()))
            .collect::<Vec<_>>()
    };
    let serial = collected(1);
    assert_eq!(collected(4), serial, "parallel collect differs from serial");
    let mut streamed = configure(Walker::new(root))
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

#[test]
fn collect_retains_a_disappearing_root_error() {
    let fixture = Fixture::new();
    fs::remove_dir_all(&fixture.root).expect("remove fixture root before walking");

    let result = Walker::new(&fixture.root)
        .error_policy(ErrorPolicy::Collect)
        .collect()
        .expect("collect returns partial result");
    assert_eq!(result.errors().len(), 1);
    assert_eq!(result.errors()[0].operation(), "read_dir");
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
    assert_eq!(error.operation(), "read_dir");
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
            .any(|error| error.operation() == "read_dir" && error.path() == locked)
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
