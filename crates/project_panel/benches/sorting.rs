use criterion::{Criterion, criterion_group, criterion_main};
use project::{Entry, EntryKind, GitEntry, ProjectEntryId};
use project_panel::par_sort_worktree_entries;
use settings::{ProjectPanelSortMode, ProjectPanelSortOrder};
use std::sync::Arc;
use util::rel_path::RelPath;

fn worktree_entry(path: &str, kind: EntryKind) -> GitEntry {
    GitEntry {
        entry: Entry {
            kind,
            path: Arc::from(RelPath::from_unix_str(path).expect("valid generated path")),
            id: ProjectEntryId::default(),
            size: 0,
            inode: 0,
            mtime: None,
            canonical_path: None,
            is_ignored: false,
            is_always_included: false,
            is_external: false,
            is_private: false,
            is_hidden: false,
            char_bag: Default::default(),
            is_fifo: false,
        },
        git_summary: Default::default(),
    }
}

fn large_worktree_entries() -> Vec<GitEntry> {
    let roots = [
        "arch", "docs", "drivers", "fs", "include", "kernel", "lib", "net", "scripts", "tools",
    ];
    let filenames = [
        "Kconfig",
        "Makefile",
        "README.md",
        "Schema.json",
        "Setup.cfg",
        "config.toml",
        "device.c",
        "device.h",
        "device_test.c",
        "entry.S",
        "interface.h",
        "layout.dts",
        "main.rs",
        "preflight.sh",
        "run.py",
        "trace.c",
        "usage.txt",
    ];

    let mut entries = vec![worktree_entry("", EntryKind::Dir)];
    for root in roots {
        entries.push(worktree_entry(root, EntryKind::Dir));
        for subsystem in 0..60 {
            let parent = format!("{root}/subsystem_{subsystem}");
            entries.push(worktree_entry(&parent, EntryKind::Dir));
            for module in 0..9 {
                let directory = format!("{parent}/module_{module}");
                entries.push(worktree_entry(&directory, EntryKind::Dir));
                for filename in filenames {
                    let path = format!("{directory}/{filename}");
                    entries.push(worktree_entry(&path, EntryKind::File));
                }
            }
        }
    }
    entries
}

fn criterion_benchmark(c: &mut Criterion) {
    let entries = large_worktree_entries();

    let modes = [
        ("DirectoriesFirst", ProjectPanelSortMode::DirectoriesFirst),
        ("Mixed", ProjectPanelSortMode::Mixed),
        ("FilesFirst", ProjectPanelSortMode::FilesFirst),
    ];
    let orders = [
        ("Default", ProjectPanelSortOrder::Default),
        ("Upper", ProjectPanelSortOrder::Upper),
        ("Lower", ProjectPanelSortOrder::Lower),
        ("Unicode", ProjectPanelSortOrder::Unicode),
        ("Extension", ProjectPanelSortOrder::Extension),
    ];

    for (mode_name, mode) in &modes {
        for (order_name, order) in &orders {
            c.bench_function(
                &format!("Sort large worktree ({mode_name}, {order_name})"),
                |b| {
                    b.iter_batched(
                        || entries.clone(),
                        |mut entries| par_sort_worktree_entries(&mut entries, *mode, *order),
                        criterion::BatchSize::LargeInput,
                    );
                },
            );
        }
    }
}

criterion_group!(benches, criterion_benchmark);
criterion_main!(benches);
