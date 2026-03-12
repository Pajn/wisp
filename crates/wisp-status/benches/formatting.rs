use criterion::{Criterion, criterion_group, criterion_main};
use wisp_core::{AttentionBadge, StatusSessionItem};
use wisp_status::{StatusFormatOptions, format_status_line};

fn seeded_items() -> Vec<StatusSessionItem> {
    (0..50)
        .map(|index| StatusSessionItem {
            label: format!("session-{index}"),
            is_current: index == 0,
            is_previous: index == 1,
            badge: if index % 11 == 0 {
                AttentionBadge::Bell
            } else if index % 5 == 0 {
                AttentionBadge::Activity
            } else {
                AttentionBadge::None
            },
        })
        .collect()
}

fn bench_status_formatting(criterion: &mut Criterion) {
    let items = seeded_items();
    criterion.bench_function("format_status_line", |bench| {
        bench.iter(|| format_status_line(&items, &StatusFormatOptions::default()));
    });
}

criterion_group!(benches, bench_status_formatting);
criterion_main!(benches);
