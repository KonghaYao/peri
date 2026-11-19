//! tui-chart gallery for the storybook: one variant per upstream example.
//!
//! Demo data and widget configuration are adapted from tui-chart 0.6.6
//! (`examples/*.rs`), MIT licensed:
//!
//! MIT License
//!
//! Copyright (c) 2025 Sorin Albu-Irimies
//!
//! Permission is hereby granted, free of charge, to any person obtaining a copy
//! of this software and associated documentation files (the "Software"), to deal
//! in the Software without restriction, including without limitation the rights
//! to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
//! copies of the Software, and to permit persons to whom the Software is
//! furnished to do so, subject to the following conditions:
//!
//! The above copyright notice and this permission notice shall be included in all
//! copies or substantial portions of the Software.
//!
//! THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
//! IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
//! FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
//! AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
//! LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
//! OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
//! SOFTWARE.
//!
//! Every demo is a deterministic, non-animated frame: no clock and no
//! terminal I/O. Rectangles smaller than [`MIN_WIDTH`] × [`MIN_HEIGHT`] are
//! skipped so the storybook can render at any terminal size.

mod dashboard;
mod simple;

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::widgets::Block;

/// Smallest rectangle a demo attempts to draw into.
const MIN_WIDTH: u16 = 12;
const MIN_HEIGHT: u16 = 4;

/// One gallery variant: the upstream example name, a one-line note and the
/// frame renderer.
pub(crate) struct Demo {
    pub(crate) key: &'static str,
    pub(crate) note: &'static str,
    draw: fn(Rect, &mut Buffer),
}

/// Gallery variants in upstream example order.
pub(crate) const DEMOS: &[Demo] = &[
    Demo {
        key: "bars",
        note: "Grouped quarterly bars with value labels.",
        draw: simple::bars,
    },
    Demo {
        key: "boxplot",
        note: "Four sampled groups, mean and outliers shown.",
        draw: simple::boxplot,
    },
    Demo {
        key: "bubble",
        note: "Three markets, bubble size is users.",
        draw: simple::bubble,
    },
    Demo {
        key: "bullet",
        note: "Four KPIs against targets and ranges.",
        draw: simple::bullet,
    },
    Demo {
        key: "calendar",
        note: "Half-year contribution calendar with legend.",
        draw: simple::calendar,
    },
    Demo {
        key: "candlestick",
        note: "200 candles with volume and a SMA20 overlay.",
        draw: simple::candlestick,
    },
    Demo {
        key: "chartstack",
        note: "Three weighted panes sharing one stack.",
        draw: simple::chartstack,
    },
    Demo {
        key: "depth",
        note: "Order-book depth with mid price and spread.",
        draw: simple::depth,
    },
    Demo {
        key: "funnel",
        note: "Conversion funnel with taper and percentages.",
        draw: simple::funnel,
    },
    Demo {
        key: "gantt",
        note: "Project schedule with a today marker.",
        draw: simple::gantt,
    },
    Demo {
        key: "gauge",
        note: "Linear gauge with bands and a target.",
        draw: simple::gauge,
    },
    Demo {
        key: "heatmap",
        note: "Seven days by 24 hours on the Viridis ramp.",
        draw: simple::heatmap,
    },
    Demo {
        key: "histogram",
        note: "2000 samples, 12 bins, KDE and mean/median.",
        draw: simple::histogram,
    },
    Demo {
        key: "line",
        note: "Three series with bands, annotations and legend.",
        draw: simple::line,
    },
    Demo {
        key: "lollipop",
        note: "Sorted language satisfaction scores.",
        draw: simple::lollipop,
    },
    Demo {
        key: "paired_bars",
        note: "Before/after pairs in the inline layout.",
        draw: simple::paired_bars,
    },
    Demo {
        key: "pie",
        note: "Language share with a legend.",
        draw: simple::pie,
    },
    Demo {
        key: "radar",
        note: "Two filled series over six axes.",
        draw: simple::radar,
    },
    Demo {
        key: "radial",
        note: "Three concentric activity rings.",
        draw: simple::radial,
    },
    Demo {
        key: "renko",
        note: "Renko bricks from a 300-step random walk.",
        draw: simple::renko,
    },
    Demo {
        key: "rose",
        note: "Monthly rainfall as a polar area chart.",
        draw: simple::rose,
    },
    Demo {
        key: "sankey",
        note: "Money flow through budget and savings.",
        draw: simple::sankey,
    },
    Demo {
        key: "slope",
        note: "Popularity change between 2022 and 2025.",
        draw: simple::slope,
    },
    Demo {
        key: "sparkline",
        note: "Bar, area, line and win/loss sparklines.",
        draw: simple::sparkline,
    },
    Demo {
        key: "stacked_area",
        note: "Four services stacked, outline on.",
        draw: simple::stacked_area,
    },
    Demo {
        key: "stream",
        note: "Rolling price with a volume pane.",
        draw: simple::stream,
    },
    Demo {
        key: "treemap",
        note: "Two groups of four languages, values shown.",
        draw: simple::treemap,
    },
    Demo {
        key: "violin",
        note: "Unimodal, bimodal and wide distributions.",
        draw: simple::violin,
    },
    Demo {
        key: "waterfall",
        note: "Quarterly deltas from an initial 100.",
        draw: simple::waterfall,
    },
    Demo {
        key: "instruments",
        note: "Oscilloscope ECG panel (upstream page 1).",
        draw: dashboard::instruments,
    },
    Demo {
        key: "cyber",
        note: "Glow, chamfered frames and instrument panels.",
        draw: dashboard::cyber,
    },
    Demo {
        key: "themes",
        note: "Eight widgets sharing the storybook theme.",
        draw: dashboard::themes,
    },
    Demo {
        key: "showcase",
        note: "Five chart types in one dashboard.",
        draw: dashboard::showcase,
    },
    Demo {
        key: "interactive",
        note: "Line chart with one pinned hover tooltip.",
        draw: dashboard::interactive,
    },
    Demo {
        key: "bench",
        note: "Static frame from the benchmark data sets.",
        draw: dashboard::bench,
    },
];

/// Render demo `demo` into `area`; unknown indices and tiny rectangles are
/// ignored instead of panicking.
pub(crate) fn render_demo(demo: usize, area: Rect, buf: &mut Buffer) {
    let Some(demo) = DEMOS.get(demo) else {
        return;
    };
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        return;
    }
    (demo.draw)(area, buf);
}

fn block(title: &'static str) -> Block<'static> {
    Block::bordered().title(title)
}

/// Deterministic xorshift RNG copied from the upstream examples so the demo
/// data matches without an extra dependency.
struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed.max(1))
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn unit(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }

    /// Approximate standard normal (sum of uniforms).
    fn gauss(&mut self) -> f64 {
        (0..12).map(|_| self.unit()).sum::<f64>() - 6.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_all_render(width: u16, height: u16) {
        for (index, demo) in DEMOS.iter().enumerate() {
            let area = Rect::new(0, 0, width, height);
            let mut buf = Buffer::empty(area);
            render_demo(index, area, &mut buf);
            let drawn = buf
                .content()
                .iter()
                .any(|cell| !cell.symbol().trim().is_empty());
            assert!(
                drawn,
                "demo {index} ({}) rendered nothing at {width}x{height}",
                demo.key
            );
        }
    }

    #[test]
    fn every_demo_renders_at_the_default_size() {
        assert_all_render(100, 32);
    }

    #[test]
    fn every_demo_renders_in_a_small_buffer() {
        assert_all_render(40, 10);
    }

    #[test]
    fn every_demo_key_is_unique() {
        for (index, demo) in DEMOS.iter().enumerate() {
            assert!(!demo.key.is_empty());
            assert!(!demo.note.is_empty());
            assert!(
                DEMOS[index + 1..].iter().all(|other| other.key != demo.key),
                "duplicate demo key {}",
                demo.key
            );
        }
        assert_eq!(DEMOS.len(), 35);
    }

    #[test]
    fn bench_data_is_built_once_and_renders_deterministically() {
        let area = Rect::new(0, 0, 100, 32);
        let mut first = Buffer::empty(area);
        let mut second = Buffer::empty(area);
        dashboard::bench(area, &mut first);
        dashboard::bench(area, &mut second);
        assert_eq!(first.content(), second.content());
        assert!(std::ptr::eq(
            dashboard::bench_charts(),
            dashboard::bench_charts()
        ));
    }

    #[test]
    fn tiny_and_unknown_rectangles_are_ignored() {
        for index in 0..=DEMOS.len() {
            for (width, height) in [
                (0, 0),
                (0, 5),
                (5, 0),
                (1, 1),
                (5, 3),
                (11, 4),
                (12, 3),
                (12, 4),
                (12, 5),
                (15, 5),
            ] {
                let area = Rect::new(0, 0, width, height);
                let mut buf = Buffer::empty(area);
                render_demo(index, area, &mut buf);
            }
        }
    }
}
