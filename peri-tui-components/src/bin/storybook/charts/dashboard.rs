//! Multi-panel gallery demos (dashboards, instruments and benchmarks).
//!
//! Data adapted from tui-chart 0.6.6 `examples/*.rs`
//! (MIT, Copyright (c) 2025 Sorin Albu-Irimies).

use std::sync::OnceLock;

use peri_tui_components::charts::preview_theme;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Color;
use ratatui::widgets::{Block, Widget};
use tui_chart::{
    Axis, BarChart, BarGroup, BarMode, Blip, Candle, CandlestickChart, Gauge, Gradient, Heatmap,
    Histogram, HitMap, Interaction, LineChart, LineMode, Oscilloscope, Overlay, RadarChart,
    RadarScope, RadarSeries, Series, Sparkline, Spectrum, Treemap, TreemapItem, bracket_block,
    chamfer_block, glow, jitter, sma,
};

use super::{Rng, block};

pub(super) fn instruments(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    Oscilloscope::ecg(3, 360)
        .phase(0.0)
        .theme(&theme)
        .block(block(" Oscilloscope · ECG "))
        .render(area, buf);
}

pub(super) fn cyber(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let [top, bottom] =
        Layout::vertical([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(area);
    let [left, right] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(top);
    let bars: Vec<f64> = (0..20).map(|i| jitter(0.9, i, 0.0)).collect();
    let scope = Oscilloscope::ecg(3, 300)
        .phase(0.0)
        .theme(&theme)
        .block(bracket_block(" ECG "));
    let spectrum = Spectrum::new(bars)
        .theme(&theme)
        .block(chamfer_block(" SPECTRUM "));
    let radar = RadarScope::new([
        Blip::new(0.1, 0.5),
        Blip::new(0.45, 0.8),
        Blip::new(0.8, 0.3),
    ])
    .phase(0.0)
    .theme(&theme)
    .block(chamfer_block(" RADAR "));
    Block::new().style(theme.surface_style()).render(area, buf);
    spectrum.render(left, buf);
    radar.render(right, buf);
    let [scope_area, side] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(bottom);
    let [gauge_area, tree_area] =
        Layout::vertical([Constraint::Length(3), Constraint::Min(3)]).areas(side);
    scope.render(scope_area, buf);
    Gauge::new(50.0)
        .range(0.0, 100.0)
        .zones()
        .label("LOAD 50%")
        .theme(&theme)
        .block(bracket_block(" CORE "))
        .render(gauge_area, buf);
    Treemap::new([
        TreemapItem::new("net", 40.0),
        TreemapItem::new("gpu", 25.0),
        TreemapItem::new("io", 15.0),
        TreemapItem::new("cpu", 20.0),
    ])
    .chamfered(true)
    .render(tree_area, buf);
    for panel in [left, right, scope_area, side] {
        glow(buf, panel, 1, 0.22);
    }
}

pub(super) fn themes(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let rows = Layout::vertical([Constraint::Ratio(1, 2); 2]).split(area);
    let top = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(rows[0]);
    let bottom = Layout::horizontal([Constraint::Ratio(1, 4); 4]).split(rows[1]);
    let title = |name: &str| Block::bordered().title(format!(" {name} "));

    let mut rng = Rng::new(11);
    let mut price = 100.0;
    let candles: Vec<Candle> = (0..40)
        .map(|_| {
            let open = price;
            let close = open + rng.gauss();
            let high = open.max(close) + rng.unit();
            let low = open.min(close) - rng.unit();
            price = close;
            Candle::new(open, high, low, close)
        })
        .collect();
    let spark: Vec<f64> = (0..80).map(|_| rng.unit() * 10.0).collect();

    LineChart::new(vec![
        Series::new("sin").data((0..60).map(|i| (i as f64, (i as f64 / 6.0).sin()))),
        Series::new("cos").data((0..60).map(|i| (i as f64, (i as f64 / 9.0).cos()))),
    ])
    .theme(&theme)
    .block(title(&format!("Line · {}", theme.name)))
    .render(top[0], buf);
    CandlestickChart::new(candles)
        .theme(&theme)
        .block(title("Candles"))
        .render(top[1], buf);
    BarChart::new(vec![
        BarGroup::new("Q1", [120.0, 80.0]),
        BarGroup::new("Q2", [150.0, 95.0]),
        BarGroup::new("Q3", [90.0, 110.0]),
    ])
    .series_names(["A", "B"])
    .theme(&theme)
    .block(title("Bars"))
    .render(top[2], buf);
    Sparkline::new(spark)
        .theme(&theme)
        .block(title("Sparkline"))
        .render(top[3], buf);

    Gauge::new(65.0)
        .bands([(50.0, Color::Green), (80.0, Color::Yellow)])
        .label("65%")
        .theme(&theme)
        .block(title("Gauge"))
        .render(bottom[0], buf);
    RadarChart::new(["Speed", "Power", "Range", "Armor", "Cost"])
        .series(RadarSeries::new("A", [7.0, 5.0, 8.0, 4.0, 6.0]))
        .series(RadarSeries::new("B", [4.0, 8.0, 5.0, 7.0, 3.0]))
        .max(10.0)
        .theme(&theme)
        .block(title("Radar"))
        .render(bottom[1], buf);
    let grid: Vec<Vec<f64>> = (0..6)
        .map(|row| {
            (0..10)
                .map(|column| ((row * 10 + column) % 7) as f64)
                .collect()
        })
        .collect();
    Heatmap::new(grid)
        .theme(&theme)
        .block(title("Heatmap"))
        .render(bottom[2], buf);
    Treemap::new([
        TreemapItem::new("A", 40.0),
        TreemapItem::new("B", 25.0),
        TreemapItem::new("C", 20.0),
        TreemapItem::new("D", 10.0),
        TreemapItem::new("E", 5.0),
    ])
    .theme(&theme)
    .block(title("Treemap"))
    .render(bottom[3], buf);
}

pub(super) fn showcase(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(11);
    let mut candles = Vec::new();
    let mut price = 100.0;
    for index in 0..80 {
        let open = price;
        let close = open + rng.gauss();
        candles.push(
            Candle::new(
                open,
                open.max(close) + rng.unit(),
                open.min(close) - rng.unit(),
                close,
            )
            .volume(1000.0 + rng.unit() * 1000.0)
            .label(format!("{index}")),
        );
        price = close;
    }
    let samples: Vec<f64> = (0..500).map(|_| rng.gauss()).collect();
    let [top, bottom] =
        Layout::vertical([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(area);
    let [candle_area, area_area] =
        Layout::horizontal([Constraint::Percentage(55), Constraint::Percentage(45)]).areas(top);
    let [bar_area, histogram_area, heat_area] =
        Layout::horizontal([Constraint::Ratio(1, 3); 3]).areas(bottom);

    CandlestickChart::new(candles.clone())
        .show_volume(true)
        .overlay(Overlay::new("SMA10", sma(&candles, 10), Color::Yellow))
        .theme(&theme)
        .block(block(" Candlestick "))
        .render(candle_area, buf);
    LineChart::new(vec![
        Series::new("load").data((0..60).map(|i| (i as f64, (i as f64 / 6.0).sin() * 3.0 + 5.0))),
        Series::new("mem")
            .data((0..60).map(|i| (i as f64, (i as f64 / 9.0).cos() * 2.0 + 4.0)))
            .mode(LineMode::Step),
    ])
    .mode(LineMode::Area)
    .y_axis(Axis::new().grid(true))
    .theme(&theme)
    .block(block(" Area / Step "))
    .render(area_area, buf);
    BarChart::new(vec![
        BarGroup::new("A", [4.0, 2.0]),
        BarGroup::new("B", [7.0, 3.0]),
        BarGroup::new("C", [5.0, 6.0]),
    ])
    .mode(BarMode::Stacked)
    .theme(&theme)
    .block(block(" Stacked bars "))
    .render(bar_area, buf);
    Histogram::new(samples)
        .bins(10)
        .theme(&theme)
        .block(block(" Histogram "))
        .render(histogram_area, buf);
    let grid: Vec<Vec<f64>> = (0..6)
        .map(|row| {
            (0..10)
                .map(|column| ((column as f64).sin() + (row as f64).cos()) * 10.0)
                .collect()
        })
        .collect();
    Heatmap::new(grid)
        .gradient(Gradient::Heat)
        .theme(&theme)
        .block(block(" Heatmap "))
        .render(heat_area, buf);
}

pub(super) fn interactive(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(11);
    let lines: Vec<Vec<(f64, f64)>> = (0..3)
        .map(|series| {
            let mut value = 10.0 + series as f64 * 6.0;
            (0..40)
                .map(|index| {
                    value += rng.gauss() * 1.5;
                    (index as f64, value)
                })
                .collect()
        })
        .collect();
    let names = ["alpha", "beta", "gamma"];
    let series: Vec<Series> = lines
        .iter()
        .zip(names)
        .map(|(points, name)| Series::new(name).data(points.iter().copied()))
        .collect();
    let chart = LineChart::new(series)
        .x_axis(Axis::new().bounds(0.0, 39.0))
        .y_axis(Axis::new().bounds(0.0, 35.0))
        .theme(&theme)
        .block(block(" Line · hover tooltip "));
    let hits = HitMap::capture(chart, area, buf);
    let mut interaction = Interaction::default();
    if let Some(hit) = hits.hits().first() {
        // Pin one deterministic hit so the pinned frame shows the tooltip and
        // the hovered element highlight.
        interaction.on_move(hit.rect.x, hit.rect.y);
        interaction.render_tooltip(&hits, buf, area);
    }
}

pub(super) fn bench(area: Rect, buf: &mut Buffer) {
    let charts = bench_charts();
    let [top, bottom] =
        Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(area);
    let [line_area, candle_area] =
        Layout::horizontal([Constraint::Percentage(50), Constraint::Percentage(50)]).areas(top);
    let [heat_area, spark_area] =
        Layout::horizontal([Constraint::Percentage(60), Constraint::Percentage(40)]).areas(bottom);

    Widget::render(&charts.line, line_area, buf);
    Widget::render(&charts.candles, candle_area, buf);
    Widget::render(&charts.heatmap, heat_area, buf);
    Widget::render(&charts.sparkline, spark_area, buf);
}

/// Bench 图表的固定数据与配置：数据量大，进程内只构建一次，之后每次 draw 复用。
pub(super) struct BenchCharts {
    line: LineChart,
    candles: CandlestickChart,
    heatmap: Heatmap,
    sparkline: Sparkline<'static>,
}

static BENCH_CHARTS: OnceLock<BenchCharts> = OnceLock::new();

pub(super) fn bench_charts() -> &'static BenchCharts {
    BENCH_CHARTS.get_or_init(|| {
        let theme = preview_theme();
        let points: Vec<(f64, f64)> = (0..100_000)
            .map(|i| (i as f64, (i as f64 / 300.0).sin() * 50.0 + (i % 17) as f64))
            .collect();
        let candles: Vec<Candle> = (0..5000)
            .map(|i| {
                let base = 100.0 + (i as f64 / 20.0).sin() * 10.0;
                Candle::new(base, base + 3.0, base - 3.0, base + 1.0).volume(100.0 + (i % 7) as f64)
            })
            .collect();
        let rows: Vec<Vec<f64>> = (0..200)
            .map(|row| {
                (0..400)
                    .map(|column| ((column * row) % 97) as f64)
                    .collect()
            })
            .collect();
        BenchCharts {
            line: LineChart::new(vec![Series::new("a").data(points)])
                .theme(&theme)
                .block(block(" line 100k pts ")),
            candles: CandlestickChart::new(candles)
                .show_volume(true)
                .theme(&theme)
                .block(block(" candles 5k ")),
            heatmap: Heatmap::new(rows)
                .theme(&theme)
                .block(block(" heatmap 400x200 ")),
            sparkline: Sparkline::new((0..100_000).map(|i| (i % 50) as f64))
                .theme(&theme)
                .block(block(" sparkline 100k ")),
        }
    })
}
