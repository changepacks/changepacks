//! Criterion benchmarks for the pure hot-path utilities: semver bumping
//! (`next_version`), version-prefix splitting (`split_version`), and the
//! Kahn's-algorithm dependency sort (`sort_by_dependencies`).
//!
//! These functions are deterministic and allocation-light, so they provide a
//! stable regression signal for the retry-now improvement loop.

use std::hint::black_box;
use std::path::PathBuf;
use std::time::Duration;

use changepacks_core::{Package, Project, UpdateType};
use changepacks_node::package::NodePackage;
use changepacks_utils::{next_version, sort_by_dependencies, split_version};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};

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
                    black_box(sort_by_dependencies(black_box(refs)));
                });
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
    targets = bench_next_version, bench_split_version, bench_sort_by_dependencies
}
criterion_main!(benches);
