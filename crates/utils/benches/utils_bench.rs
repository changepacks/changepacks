//! Criterion benchmarks for the pure hot-path utilities: semver bumping
//! (`next_version`), version-prefix splitting (`split_version`), project
//! ordering, the Kahn's-algorithm dependency sort (`sort_by_dependencies`), and
//! the reverse-dependency expansion (`apply_reverse_dependencies`).
//!
//! These functions are deterministic and allocation-light, so they provide a
//! stable regression signal for the retry-now improvement loop.

use std::collections::HashMap;
use std::hint::black_box;
use std::path::{Path, PathBuf};
use std::time::Duration;

use changepacks_core::{ChangePackResultLog, Package, Project, UpdateType};
use changepacks_node::package::NodePackage;
use changepacks_utils::{
    apply_reverse_dependencies, next_version, sort_by_dependencies, split_version,
};
use criterion::{BatchSize, BenchmarkId, Criterion, criterion_group, criterion_main};

/// Synthetic repository root matching the absolute paths produced by
/// [`make_project`], so relative-path derivation succeeds without touching disk.
const BENCH_REPO_ROOT: &str = "/bench";

/// Build a single Node package project with the given dependency names.
fn make_project(name: &str, deps: &[String]) -> Project {
    let mut package = NodePackage::new(
        Some(name.to_string()),
        Some("1.0.0".to_string()),
        PathBuf::from(format!("/bench/{name}/package.json")),
        PathBuf::from(format!("{name}/package.json")),
    );
    for dep in deps {
        package.add_dependency(dep);
    }
    Project::Package(Box::new(package))
}

/// Build a realistic acyclic dependency graph of `n` packages where each
/// package depends on a few earlier ones, exercising both the graph
/// construction and the topological ordering.
fn build_graph(n: usize) -> Vec<Project> {
    (0..n)
        .map(|i| {
            let mut deps = Vec::new();
            if i > 0 {
                deps.push(format!("pkg{}", i - 1));
            }
            if i > 5 {
                deps.push(format!("pkg{}", i - 5));
            }
            if i > 10 {
                deps.push(format!("pkg{}", i / 2));
            }
            make_project(&format!("pkg{i}"), &deps)
        })
        .collect()
}

/// Build one directed cycle, the worst case for the old per-residual-node
/// reachability searches and the target workload for SCC detection.
fn build_cyclic_graph(n: usize) -> Vec<Project> {
    (0..n)
        .map(|i| make_project(&format!("cycle{i}"), &[format!("cycle{}", (i + 1) % n)]))
        .collect()
}

/// Build projects whose variant, language, and name are all identical, leaving
/// their deliberately shuffled relative paths to decide every comparison.
fn build_same_identity_projects(n: usize) -> Vec<Project> {
    (0..n)
        .map(|position| {
            // 811 is coprime with the benchmark's power-of-two input size, so
            // this visits every path index exactly once without producing an
            // already sorted or reversed run.
            let path_index = position * 811 % n;
            let relative_path = if path_index.is_multiple_of(2) {
                format!("packages\\{path_index:04}\\package.json")
            } else {
                format!("packages/{path_index:04}/package.json")
            };
            let package = NodePackage::new(
                Some("same-package".to_string()),
                Some("1.0.0".to_string()),
                PathBuf::from(format!("/bench/packages/{path_index:04}/package.json")),
                PathBuf::from(relative_path),
            );
            Project::Package(Box::new(package))
        })
        .collect()
}

fn bench_next_version(c: &mut Criterion) {
    c.bench_function("next_version/patch", |b| {
        b.iter(|| next_version(black_box("10.20.30"), black_box(UpdateType::Patch)).unwrap());
    });
    c.bench_function("next_version/major_build", |b| {
        b.iter(|| next_version(black_box("10.20.30+42"), black_box(UpdateType::Major)).unwrap());
    });
}

fn bench_split_version(c: &mut Criterion) {
    c.bench_function("split_version/prefixed", |b| {
        b.iter(|| black_box(split_version(black_box(">=1.0.0+build1"))));
    });
    c.bench_function("split_version/plain", |b| {
        b.iter(|| black_box(split_version(black_box("1.0.0-alpha.1+build1"))));
    });
}

fn bench_project_sort_same_language_same_name_paths(c: &mut Criterion) {
    let projects = build_same_identity_projects(1_024);
    let unsorted: Vec<&Project> = projects.iter().collect();

    c.bench_function("project_sort/same_language_same_name_paths", |b| {
        b.iter_batched(
            || unsorted.clone(),
            |mut projects| {
                projects.sort();
                black_box(projects);
            },
            BatchSize::SmallInput,
        );
    });
}

fn bench_sort_by_dependencies(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort_by_dependencies");
    for size in [16_usize, 64, 256] {
        let projects = build_graph(size);
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &projects,
            |b, projects| {
                b.iter(|| {
                    let refs: Vec<&Project> = projects.iter().collect();
                    black_box(
                        sort_by_dependencies(black_box(refs)).expect("benchmark graph is a DAG"),
                    );
                });
            },
        );
    }
    group.finish();
}

fn bench_sort_by_dependencies_cyclic(c: &mut Criterion) {
    let mut group = c.benchmark_group("sort_by_dependencies_cyclic");
    for size in [64_usize, 256, 1_024] {
        let projects = build_cyclic_graph(size);
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &projects,
            |b, projects| {
                b.iter(|| {
                    let refs: Vec<&Project> = projects.iter().collect();
                    black_box(
                        sort_by_dependencies(black_box(refs))
                            .expect_err("benchmark graph contains one cycle"),
                    );
                });
            },
        );
    }
    group.finish();
}

/// Seed the update map with one or two directly changed packages, the shape a
/// real `changepacks check` / `changepacks update` run produces before the
/// reverse-dependency worklist expands it.
fn seed_paths(size: usize) -> Vec<PathBuf> {
    let mut seeds = vec![PathBuf::from("pkg0/package.json")];
    if size > 1 {
        seeds.push(PathBuf::from(format!("pkg{}/package.json", size / 2)));
    }
    seeds
}

fn bench_apply_reverse_dependencies(c: &mut Criterion) {
    let repo_root = Path::new(BENCH_REPO_ROOT);
    let mut group = c.benchmark_group("apply_reverse_dependencies");
    for size in [16_usize, 64, 256] {
        let projects = build_graph(size);
        let input = (projects.iter().collect::<Vec<&Project>>(), seed_paths(size));
        group.bench_with_input(
            BenchmarkId::from_parameter(size),
            &input,
            |b, (projects, seeds)| {
                b.iter_batched(
                    || {
                        seeds
                            .iter()
                            .map(|path| {
                                (
                                    path.clone(),
                                    (
                                        UpdateType::Minor,
                                        vec![ChangePackResultLog::new(
                                            UpdateType::Minor,
                                            "bench seed".to_string(),
                                        )],
                                    ),
                                )
                            })
                            .collect::<HashMap<_, _>>()
                    },
                    |mut update_map| {
                        apply_reverse_dependencies(
                            &mut update_map,
                            black_box(projects),
                            black_box(repo_root),
                        )
                        .expect("benchmark projects have unique names inside the repo root");
                        black_box(update_map);
                    },
                    BatchSize::SmallInput,
                );
            },
        );
    }
    group.finish();
}

criterion_group! {
    name = benches;
    config = Criterion::default()
        .warm_up_time(Duration::from_millis(500))
        .measurement_time(Duration::from_secs(2))
        .sample_size(60);
    targets = bench_next_version, bench_split_version,
        bench_project_sort_same_language_same_name_paths, bench_sort_by_dependencies,
        bench_sort_by_dependencies_cyclic, bench_apply_reverse_dependencies
}
criterion_main!(benches);
