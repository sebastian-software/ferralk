#![forbid(unsafe_code)]
//! Shared fixtures for the benchmark entry points.

use std::{
    fs,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

pub const TYPESCRIPT_PATTERN: &str = "**/*.{ts,tsx}";
pub const SCOPED_TYPESCRIPT_PATTERN: &str = "{src,packages}/**/*.{ts,tsx}";
pub const NODE_MODULES_EXCLUDE: &str = "**/node_modules/**";

const NODE_MODULES_GITIGNORE: &[u8] = include_bytes!("../fixtures/palamedes.gitignore");

/// A JavaScript repository whose file count sits primarily in dependencies.
///
/// The full engine comparison, the per-pull-request Ferralk signal, and the
/// user-space CPU harness share this shape so they exercise the same work.
pub struct RepositoryFixture {
    root: PathBuf,
    files: usize,
    sources: usize,
}

impl RepositoryFixture {
    #[must_use]
    pub fn new() -> Self {
        let unique = format!(
            "ferralk-repository-bench-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock is after epoch")
                .as_nanos()
        );
        let root = std::env::temp_dir().join(unique);
        let mut files = 0;
        let mut sources = 0;

        // Application sources: what the query is actually looking for.
        for area in 0..40 {
            for module in 0..10 {
                let directory = root
                    .join("src")
                    .join(format!("area-{area}"))
                    .join(format!("module-{module}"));
                fs::create_dir_all(&directory).expect("create source directory");
                for index in 0..10 {
                    let (name, is_source) = match index % 5 {
                        0 => (format!("view-{index}.tsx"), true),
                        1 | 2 => (format!("unit-{index}.ts"), true),
                        3 => (format!("style-{index}.css"), false),
                        _ => (format!("legacy-{index}.js"), false),
                    };
                    write(&directory.join(name));
                    files += 1;
                    sources += usize::from(is_source);
                }
            }
        }

        // A second source root makes scoped-query pruning cover several roots.
        for package in 0..20 {
            let directory = root
                .join("packages")
                .join(format!("pkg-{package}"))
                .join("src");
            fs::create_dir_all(&directory).expect("create package directory");
            for index in 0..20 {
                let is_source = index % 2 == 0;
                let name = if is_source {
                    format!("index-{index}.ts")
                } else {
                    format!("index-{index}.js")
                };
                write(&directory.join(name));
                files += 1;
                sources += usize::from(is_source);
            }
        }

        // Dependencies dominate the tree, as they do in the motivating shape.
        // Every tenth package has dependencies of its own.
        for package in 0_usize..400 {
            let package_root = root.join("node_modules").join(format!("dep-{package}"));
            files += write_package(&package_root, 100);
            if package.is_multiple_of(10) {
                for nested in 0..5 {
                    let nested_root = package_root
                        .join("node_modules")
                        .join(format!("nested-{nested}"));
                    files += write_package(&nested_root, 40);
                }
            }
        }

        fs::write(root.join(".gitignore"), NODE_MODULES_GITIGNORE)
            .expect("write benchmark gitignore");
        files += 1;

        Self {
            root,
            files,
            sources,
        }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    #[must_use]
    pub const fn files(&self) -> usize {
        self.files
    }

    #[must_use]
    pub const fn sources(&self) -> usize {
        self.sources
    }
}

impl RepositoryFixture {
    /// Hands the tree to the caller and cancels its removal, so another
    /// process can walk it.
    ///
    /// The Callgrind harness needs this: building 53,601 files inside the
    /// measured region would bury the walk under fixture construction, and
    /// building it under Valgrind at all would cost minutes. The workflow
    /// therefore builds the tree once, natively, and both measured processes
    /// walk that same tree. The caller owns the directory from then on.
    #[must_use]
    pub fn keep(self) -> PathBuf {
        let fixture = std::mem::ManuallyDrop::new(self);
        fixture.root.clone()
    }
}

impl Default for RepositoryFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for RepositoryFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn write_package(package_root: &Path, files: usize) -> usize {
    let directory = package_root.join("lib");
    fs::create_dir_all(&directory).expect("create dependency directory");
    write(&package_root.join("package.json"));
    write(&package_root.join("README.md"));
    for index in 0..files {
        let name = match index % 10 {
            0 => format!("types-{index}.d.ts"),
            1 => format!("meta-{index}.json"),
            _ => format!("chunk-{index}.js"),
        };
        write(&directory.join(name));
    }
    files + 2
}

fn write(path: &Path) {
    fs::write(path, b"fixture").expect("write fixture file");
}

/// An application shape whose cost is Git-ignore rule evaluation rather than
/// directory count.
///
/// The repository fixture above carries one root rule, `node_modules/`, which
/// a walk resolves once per directory and which prunes an entire subtree. That
/// is the cheap case. This shape is the expensive one: rule files at three
/// nesting levels, every one of them consulted for entries below it, and
/// negations that stop a covering directory rule from pruning — so the walk
/// has to open an ignored directory and decide per entry inside it.
///
/// Negations are the reason this cannot be folded into the repository shape.
/// `dist/` alone prunes; `dist/` plus `!dist/keep-*.ts` cannot, and the
/// difference is exactly the work a rule engine has to do.
pub struct GitIgnoreFixture {
    root: PathBuf,
    files: usize,
}

impl GitIgnoreFixture {
    #[must_use]
    pub fn new() -> Self {
        let root = unique_root("ferralk-gitignore-bench");
        let mut files = 0;

        fs::create_dir_all(&root).expect("create gitignore benchmark root");
        write_bytes(
            &root.join(".gitignore"),
            b"build/\n*.log\nnode_modules/\n!keep.log\ncoverage/\n",
        );
        files += 1;

        for app in 0..16 {
            let app_root = root.join("apps").join(format!("app-{app}"));
            fs::create_dir_all(&app_root).expect("create application directory");
            // A covering `dist/` rule that a negation reopens: the walk must
            // descend into an ignored directory to find the re-admitted files.
            write_bytes(
                &app_root.join(".gitignore"),
                b"dist/\n.cache/\n*.snap\n!dist/keep-0.ts\n!dist/keep-1.ts\n!*.public.snap\n",
            );
            files += 1;

            for feature in 0..10 {
                let directory = app_root.join("src").join(format!("feature-{feature}"));
                fs::create_dir_all(&directory).expect("create feature directory");
                // One more rule level, so an entry deep in the tree is decided
                // by three files rather than one.
                write_bytes(&directory.join(".gitignore"), b"*.tmp\n!keep.tmp\n");
                files += 1;
                for index in 0..12 {
                    let name = match index % 6 {
                        0 => format!("view-{index}.tsx"),
                        1 | 2 => format!("unit-{index}.ts"),
                        3 => format!("trace-{index}.log"),
                        4 => format!("scratch-{index}.tmp"),
                        _ => format!("story-{index}.snap"),
                    };
                    write_bytes(&directory.join(name), b"fixture");
                    files += 1;
                }
                write_bytes(&directory.join("keep.tmp"), b"fixture");
                files += 1;
            }

            let dist = app_root.join("dist");
            fs::create_dir_all(&dist).expect("create dist directory");
            for index in 0..40 {
                write_bytes(&dist.join(format!("chunk-{index}.js")), b"fixture");
                files += 1;
            }
            // Re-admitted by the negation above, so the ignored directory has
            // to be opened and read rather than pruned.
            for index in 0..2 {
                write_bytes(&dist.join(format!("keep-{index}.ts")), b"fixture");
                files += 1;
            }

            let cache = app_root.join(".cache");
            fs::create_dir_all(&cache).expect("create cache directory");
            for index in 0..30 {
                write_bytes(&cache.join(format!("entry-{index}.bin")), b"fixture");
                files += 1;
            }
        }

        for package in 0..24 {
            let package_root = root.join("packages").join(format!("pkg-{package}"));
            let source = package_root.join("src");
            fs::create_dir_all(&source).expect("create package source directory");
            write_bytes(
                &package_root.join(".gitignore"),
                b"coverage/\n*.map\n!index.map\n",
            );
            files += 1;
            for index in 0..24 {
                let name = match index % 4 {
                    0 | 1 => format!("index-{index}.ts"),
                    2 => format!("bundle-{index}.map"),
                    _ => format!("legacy-{index}.js"),
                };
                write_bytes(&source.join(name), b"fixture");
                files += 1;
            }
            write_bytes(&source.join("index.map"), b"fixture");
            files += 1;

            let coverage = package_root.join("coverage");
            fs::create_dir_all(&coverage).expect("create coverage directory");
            for index in 0..20 {
                write_bytes(&coverage.join(format!("report-{index}.json")), b"fixture");
                files += 1;
            }
        }

        Self { root, files }
    }

    #[must_use]
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every file written, ignored or not. What a Git-ignore-respecting walk
    /// keeps is asserted by the benches against the walker's own output rather
    /// than recounted here, where directories and rule files would make a
    /// second count disagree for uninteresting reasons.
    #[must_use]
    pub const fn files(&self) -> usize {
        self.files
    }
}

impl Default for GitIgnoreFixture {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for GitIgnoreFixture {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn unique_root(prefix: &str) -> PathBuf {
    let unique = format!(
        "{prefix}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system clock is after epoch")
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn write_bytes(path: &Path, contents: &[u8]) {
    fs::write(path, contents).expect("write fixture file");
}
