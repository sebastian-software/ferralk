//! The order [`WalkOptions::sort`](crate::WalkOptions::sort) promises, and a
//! cheaper way to produce it.
//!
//! The promise is `Path::cmp` order. `Path::cmp` compares component by
//! component, which is what lets `a/./b`, `a//b` and `a/b` compare equal, but
//! it pays for that on every comparison: it re-parses both paths into
//! components and was most of the time a sorted walk spent sorting.
//!
//! Walked paths never need that normalization below their root. Every path a
//! walk produces is its root's spelling followed by names pushed onto it (see
//! [`push_entry_name`](crate::push_entry_name)), and a name from a directory
//! listing is never empty, never `.` or `..`, and never holds a separator. So
//! for one root:
//!
//! - every entry path starts with the same bytes, up to
//!   [`RootPlan::relative_start`](crate::RootPlan), and parses into the same
//!   leading components there, whatever the caller spelled - `./x//y/`, `x/.`,
//!   `/` or the empty path;
//! - what follows is `name/name/.../name`, whose components are exactly those
//!   names.
//!
//! `Path::cmp` therefore reduces to comparing the name sequences after that
//! offset lexicographically, and comparing name sequences lexicographically is
//! comparing their `/`-joined bytes with `/` ranked below every other byte: at
//! the first difference either both sides are inside a name, or one name has
//! ended where the other goes on, and the ended one sorts first.
//!
//! Where that argument does not hold the sort keeps `Path::cmp`: across
//! several roots, whose spellings share no prefix, and off Unix, where `/` and
//! `\` both separate and a prefix such as `C:` has rules of its own.

use std::cmp::Ordering;

use crate::{WalkEntry, Walker};

/// Sorts `entries` into `Path::cmp` order, the way `sort(true)` promises.
pub(crate) fn sort_entries(walker: &Walker, entries: &mut [WalkEntry]) {
    match shared_prefix_len(walker) {
        Some(start) => entries.sort_by(|left, right| {
            compare_below_root(left.path_bytes(), right.path_bytes(), start)
        }),
        None => entries.sort_by(|left, right| left.path.cmp(&right.path)),
    }
}

/// The byte length every entry path of this walk shares and after which only
/// listed names follow, or `None` when the byte order is not known to be the
/// component order.
fn shared_prefix_len(walker: &Walker) -> Option<usize> {
    match walker.roots.as_slice() {
        [root] if cfg!(unix) => Some(root.relative_start),
        _ => None,
    }
}

/// Compares two walked paths from the same root by the names below it.
///
/// `start` is the root's [`RootPlan::relative_start`](crate::RootPlan): the
/// bytes before it are the same in every such path, so only the rest is read.
/// The root itself, were it ever compared, is shorter than `start` and has no
/// names, which puts it first, as `Path::cmp` does.
fn compare_below_root(left: &[u8], right: &[u8], start: usize) -> Ordering {
    let left = left.get(start..).unwrap_or_default();
    let right = right.get(start..).unwrap_or_default();
    let common = common_prefix_len(left, right);
    match (left.get(common), right.get(common)) {
        (Some(&left), Some(&right)) => separator_first(left).cmp(&separator_first(right)),
        (left, right) => left.is_some().cmp(&right.is_some()),
    }
}

/// A byte's rank when the separator comes before everything a name can hold.
fn separator_first(byte: u8) -> u16 {
    if byte == b'/' { 0 } else { u16::from(byte) + 1 }
}

/// How many leading bytes `left` and `right` share.
///
/// Sibling paths share their whole directory, so the comparison mostly runs
/// through equal bytes; eight at a time is what keeps that cheap.
fn common_prefix_len(left: &[u8], right: &[u8]) -> usize {
    const WORD: usize = std::mem::size_of::<u64>();
    let limit = left.len().min(right.len());
    let mut index = 0;
    while index + WORD <= limit && left[index..index + WORD] == right[index..index + WORD] {
        index += WORD;
    }
    while index < limit && left[index] == right[index] {
        index += 1;
    }
    index
}

#[cfg(all(test, unix))]
mod tests {
    use std::{
        ffi::OsStr,
        fs,
        os::unix::ffi::OsStrExt,
        path::{Path, PathBuf},
        sync::atomic::{AtomicUsize, Ordering as AtomicOrdering},
        time::{SystemTime, UNIX_EPOCH},
    };

    use super::*;
    use crate::{RootPlan, WalkOptions, push_entry_name};

    /// A small deterministic generator, so a failure names a case that
    /// reproduces rather than one that went away.
    struct SplitMix(u64);

    impl SplitMix {
        fn next(&mut self) -> u64 {
            self.0 = self.0.wrapping_add(0x9e37_79b9_7f4a_7c15);
            let mut mixed = self.0;
            mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xbf58_476d_1ce4_e5b9);
            mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94d0_49bb_1331_11eb);
            mixed ^ (mixed >> 31)
        }

        fn below(&mut self, bound: usize) -> usize {
            (self.next() % bound as u64) as usize
        }

        fn pick<'a, T>(&mut self, choices: &'a [T]) -> &'a T {
            &choices[self.below(choices.len())]
        }
    }

    /// Bytes chosen around the separator's rank: those just below `/` (space,
    /// `!`, `-`, `.`), `/` + 1, and the high bytes that are not UTF-8.
    const NAME_BYTES: &[u8] = b"\x01 !-.0a\xa0\xff";

    /// Roots spelled every way a caller may: repeated, trailing and missing
    /// separators, `.` and `..` components, a leading `./`, and non-UTF-8.
    const ROOTS: &[&[u8]] = &[
        b"",
        b".",
        b"./",
        b"..",
        b"../",
        b"/",
        b"//",
        b"x",
        b"x/",
        b"x//",
        b"x/.",
        b"x/./",
        b"x/..",
        b"./x//y/",
        b"./x/./y/.",
        b"/x//./y//",
        b"..//x",
        b"\xff/.\xa0/",
        b"x-/",
        b"x.",
    ];

    /// A name a directory listing can return: non-empty, never `.` or `..`,
    /// and without a separator. Short and drawn from few bytes so that names
    /// often share prefixes, which is where the orders could part.
    fn listed_name(random: &mut SplitMix) -> Vec<u8> {
        loop {
            let length = 1 + random.below(3);
            let name: Vec<u8> = (0..length).map(|_| *random.pick(NAME_BYTES)).collect();
            if name != b"." && name != b".." {
                return name;
            }
        }
    }

    /// A path the walk could produce under `root`, built the way it builds
    /// one.
    fn walked_path(random: &mut SplitMix, root: &[u8]) -> PathBuf {
        let mut path = PathBuf::from(OsStr::from_bytes(root));
        for _ in 0..random.below(4) {
            push_entry_name(&mut path, OsStr::from_bytes(&listed_name(random)));
        }
        path
    }

    #[test]
    fn byte_order_below_the_root_agrees_with_path_cmp_on_walked_paths() {
        let mut random = SplitMix(0x0404);
        for &root in ROOTS {
            let start = RootPlan::new(PathBuf::from(OsStr::from_bytes(root))).relative_start;
            let paths: Vec<PathBuf> = (0..160).map(|_| walked_path(&mut random, root)).collect();
            for left in &paths {
                for right in &paths {
                    let fast = compare_below_root(
                        left.as_os_str().as_bytes(),
                        right.as_os_str().as_bytes(),
                        start,
                    );
                    assert_eq!(
                        fast,
                        left.cmp(right),
                        "root {:?}: {left:?} vs {right:?}",
                        OsStr::from_bytes(root)
                    );
                }
            }
        }
    }

    /// The test above has teeth only if plain byte order would have failed
    /// it. These pairs are where it does: a byte below `/` inside a name, and
    /// spellings that differ in bytes but not in components.
    #[test]
    fn plain_byte_order_is_not_path_cmp_order() {
        let start = RootPlan::new(PathBuf::from("./x//y/")).relative_start;
        let (left, right) = (Path::new("./x//y/a/b"), Path::new("./x//y/a-b"));
        assert_eq!(left.cmp(right), Ordering::Less);
        assert_eq!(
            left.as_os_str()
                .as_bytes()
                .cmp(right.as_os_str().as_bytes()),
            Ordering::Greater
        );
        assert_eq!(
            compare_below_root(
                left.as_os_str().as_bytes(),
                right.as_os_str().as_bytes(),
                start
            ),
            Ordering::Less
        );
        // The root's spelling is where bytes and components disagree most,
        // and it is the part the comparison skips.
        assert_eq!(
            Path::new("./x//y/a").cmp(Path::new("x/y/a")),
            Ordering::Less
        );
        assert_eq!(Path::new("x//y").cmp(Path::new("x/./y")), Ordering::Equal);
    }

    #[test]
    fn only_a_single_root_walk_takes_the_byte_order() {
        let single = Walker::new("./x//y/");
        assert_eq!(shared_prefix_len(&single), Some("./x//y/".len()));
        let several = Walker::new("x").add_root("y").expect("add root");
        assert_eq!(shared_prefix_len(&several), None);
    }

    static NEXT_FIXTURE: AtomicUsize = AtomicUsize::new(0);

    struct Fixture {
        root: PathBuf,
    }

    impl Fixture {
        /// A tree whose names sit on both sides of `/` in byte order, nested
        /// so that a directory `a` and its siblings `a-b`, `a.b` interleave
        /// differently under byte and component order.
        fn awkward() -> Self {
            let root = std::env::temp_dir().join(format!(
                "ferralk-sort-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .expect("system clock is after unix epoch")
                    .as_nanos()
                    + NEXT_FIXTURE.fetch_add(1, AtomicOrdering::Relaxed) as u128
            ));
            for directory in ["a", "a/b", "a/b-c", "a-b", "a b", "a.b/c", "a!/x", "b"] {
                fs::create_dir_all(root.join(directory)).expect("create fixture directory");
            }
            for file in [
                "a/-", "a/0", "a/b/c", "a/b-c/d", "a/b.c", "a-", "a.", "a!/x/y", "a b/z", "a0",
                ".hidden",
            ] {
                fs::write(root.join(file), b"fixture").expect("write fixture file");
            }
            // macOS file systems refuse names that are not UTF-8; Linux keeps
            // them, and there they must sort like any other bytes.
            let _ = fs::write(root.join(OsStr::from_bytes(b"a\xff")), b"fixture");
            let _ = fs::create_dir(root.join(OsStr::from_bytes(b"a/\xa0")));
            Self { root }
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    /// The root respelled with `..`, `.` and repeated separators, relative to
    /// the current directory and led by `./`, and with trailing separators -
    /// every spelling names the same directory.
    fn awkward_spellings(root: &Path) -> Vec<PathBuf> {
        let root = root.as_os_str().as_bytes();
        let name = root
            .rsplit(|&byte| byte == b'/')
            .next()
            .expect("root has a name");
        let upward = std::env::current_dir()
            .expect("current directory")
            .components()
            .skip(1)
            .map(|_| "../")
            .collect::<String>();
        let spellings: [Vec<u8>; 6] = [
            root.to_vec(),
            [root, b"/"].concat(),
            [root, b"//./"].concat(),
            [root, b"/."].concat(),
            [root, b"/../", name, b"//"].concat(),
            [&b"./"[..], upward.as_bytes(), &root[1..], b"/./"].concat(),
        ];
        spellings
            .into_iter()
            .map(|bytes| PathBuf::from(OsStr::from_bytes(&bytes)))
            .collect()
    }

    #[test]
    fn sorted_walks_keep_path_cmp_order_under_awkward_roots() {
        let fixture = Fixture::awkward();
        for root in awkward_spellings(&fixture.root) {
            for threads in [1, 4] {
                let result = Walker::new(&root)
                    .threads(threads)
                    .options(WalkOptions::default().sort(true))
                    .collect()
                    .expect("walk the fixture");
                assert!(
                    result.errors().is_empty(),
                    "{root:?}: {:?}",
                    result.errors()
                );
                let sorted: Vec<&Path> = result.entries().iter().map(WalkEntry::path).collect();
                let mut expected = sorted.clone();
                expected.sort();
                assert!(sorted.len() >= 20, "{root:?} walked the whole fixture");
                assert_eq!(sorted, expected, "{root:?} on {threads} threads");
            }
        }
    }

    #[test]
    fn sorted_multi_root_walks_keep_path_cmp_order() {
        let fixture = Fixture::awkward();
        let result = Walker::new(fixture.root.join("a/"))
            .add_root(fixture.root.join("a-b"))
            .expect("add root")
            .add_root(fixture.root.join("./a"))
            .expect("add root")
            .options(WalkOptions::default().sort(true))
            .collect()
            .expect("walk the fixture");
        let sorted: Vec<&Path> = result.entries().iter().map(WalkEntry::path).collect();
        let mut expected = sorted.clone();
        expected.sort();
        assert_eq!(sorted, expected);
    }
}
