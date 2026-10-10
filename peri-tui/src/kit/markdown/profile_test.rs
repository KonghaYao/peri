use super::*;

const TEST_BASE_FG: Color = Color::White;

fn flatten(segments: &[MarkdownSegment]) -> Vec<ratatui::text::Line<'static>> {
    segments
        .iter()
        .flat_map(|segment| match segment {
            MarkdownSegment::Text(lines) => lines.clone(),
            MarkdownSegment::Table(_) => vec![],
            MarkdownSegment::Image(image) => image.lines.clone(),
        })
        .collect()
}

#[test]
#[serial_test::serial]
#[ignore = "release synthetic profile; run explicitly with --ignored"]
fn test_release_synthetic_matrix() {
    use crate::kit::acp_bridge::{
        perf_counters, reset_perf_counters, run_synthetic_eager_burst,
        run_synthetic_scheduler_burst,
    };

    fn shape(name: &str, bytes: usize) -> String {
        let pattern = match name {
            "prose" => "word word word word\n\n",
            "long-line" => "abcdefghijklmnopqrstuvwxyz0123456789",
            "fence" => "```text\ncode code code\n",
            "table" => "| a | b |\n| - | - |\n| c | d |\n",
            "image-like" => "![generated](relative.png)\n\n",
            _ => unreachable!(),
        };
        pattern.repeat(bytes.div_ceil(pattern.len()))[..bytes].to_string()
    }

    println!(
        "STRUCTURAL bytes,chunk,chunks,baseline_publications,post_burst_publications,baseline_projection_bytes,post_burst_projection_bytes"
    );
    for bytes in [64usize * 1024, 256 * 1024, 1024 * 1024] {
        for chunk_bytes in [16, 128, 1024, bytes] {
            let chunks = bytes.div_ceil(chunk_bytes);
            let baseline = run_synthetic_eager_burst(bytes, chunk_bytes);
            let baseline_publications = baseline.projections;
            let baseline_projection_bytes = baseline.projection_copied_bytes;
            let (post_publications, counters) = run_synthetic_scheduler_burst(bytes, chunk_bytes);
            let post_projection_bytes = counters.projection_copied_bytes;
            assert_eq!(baseline_publications as usize, chunks);
            assert_eq!(counters.projections, post_publications);
            assert!(post_projection_bytes <= bytes.saturating_add(chunk_bytes.min(bytes)) as u64);
            println!(
                "STRUCTURAL {bytes},{chunk_bytes},{chunks},{baseline_publications},{post_publications},{baseline_projection_bytes},{post_projection_bytes}"
            );
        }
    }

    println!(
        "RENDER_SAMPLE bytes,shape,width,full_parses,full_bytes,tail_parses,tail_bytes,materialized_lines"
    );
    for bytes in [64usize * 1024] {
        for shape_name in ["prose", "long-line", "fence", "table", "image-like"] {
            let fixture = shape(shape_name, bytes);
            assert_eq!(fixture.len(), bytes);
            for width in [40, 80, 160] {
                reset_perf_counters();
                let mut cache = MarkdownRenderCache::default();
                let _ = parse_markdown_cached(
                    &fixture,
                    width,
                    Palette::default(),
                    TEST_BASE_FG,
                    &mut cache,
                );
                let counters = perf_counters();
                println!(
                    "RENDER_SAMPLE {bytes},{shape_name},{width},{},{},{},{},{}",
                    counters.full_parses,
                    counters.full_parsed_bytes,
                    counters.tail_parses,
                    counters.tail_parsed_bytes,
                    counters.materialized_lines
                );
            }
        }
    }

    println!(
        "STREAMING bytes,shape,width,chunk,model_full_parses,model_full_bytes,model_materialized,model_wrap,post_full_parses,post_full_bytes,post_tail_parses,post_tail_bytes,post_materialized,post_wrap"
    );
    for shape_name in ["prose", "long-line", "fence", "table", "image-like"] {
        let bytes = 64usize * 1024;
        let chunk_bytes = 4096usize;
        let fixture = shape(shape_name, bytes);
        for width in [40u16, 80, 160] {
            reset_perf_counters();
            for end in (chunk_bytes..bytes).step_by(chunk_bytes).chain([bytes]) {
                let segments = parse_markdown(
                    &fixture[..end],
                    usize::from(width),
                    Palette::default(),
                    TEST_BASE_FG,
                );
                let lines = flatten(&segments);
                crate::kit::message_area::measure_synthetic_wrap(&lines, width);
            }
            let model = perf_counters();

            reset_perf_counters();
            let mut cache = MarkdownRenderCache::default();
            for end in (chunk_bytes..bytes).step_by(chunk_bytes).chain([bytes]) {
                let rendered = parse_markdown_chunks_cached(
                    &fixture[..end],
                    usize::from(width),
                    Palette::default(),
                    TEST_BASE_FG,
                    &mut cache,
                );
                let mut lines = rendered
                    .stable
                    .iter()
                    .flat_map(|chunk| flatten(chunk))
                    .collect::<Vec<_>>();
                lines.extend(flatten(&rendered.tail));
                crate::kit::message_area::measure_synthetic_wrap(&lines, width);
            }
            let post = perf_counters();
            assert_eq!(post.full_parses, 0);
            println!(
                "STREAMING {bytes},{shape_name},{width},{chunk_bytes},{},{},{},{},{},{},{},{},{},{}",
                model.full_parses,
                model.full_parsed_bytes,
                model.materialized_lines,
                model.wrap_recalculated_lines,
                post.full_parses,
                post.full_parsed_bytes,
                post.tail_parses,
                post.tail_parsed_bytes,
                post.materialized_lines,
                post.wrap_recalculated_lines,
            );
        }
    }

    println!("HISTORY slots,prefix_entries,aggregate_allocations,aggregate_copied_items");
    for slots in [10usize, 100, 1000] {
        reset_perf_counters();
        let (logical_entries, visual_entries) =
            crate::kit::message_area::run_synthetic_slot_index(slots);
        let counters = perf_counters();
        assert_eq!((logical_entries, visual_entries), (slots + 1, slots + 1));
        assert_eq!(counters.aggregate_allocations, 0);
        assert_eq!(counters.aggregate_copied_items, 0);
        println!(
            "HISTORY {slots},{},{},{}",
            logical_entries + visual_entries,
            counters.aggregate_allocations,
            counters.aggregate_copied_items
        );
    }
}
