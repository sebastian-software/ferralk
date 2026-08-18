#![forbid(unsafe_code)]

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

#[cfg(target_os = "linux")]
use ferralk::WalkOptions;
use ferralk::{ErrorPolicy, WalkOptions, Walker};
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
#[cfg(target_os = "linux")]
use std::path::Path;

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

    #[cfg(target_os = "linux")]
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
