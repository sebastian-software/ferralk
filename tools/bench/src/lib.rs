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
