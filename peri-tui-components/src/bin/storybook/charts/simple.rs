//! Single-widget gallery demos (one per upstream example).
//!
//! Data adapted from tui-chart 0.6.6 `examples/*.rs`
//! (MIT, Copyright (c) 2025 Sorin Irimies).

use peri_tui_components::charts::preview_theme;
use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Flex, Layout, Rect};
use ratatui::style::Color;
use ratatui::widgets::{Block, Widget};
use tui_chart::piechart::Resolution;
use tui_chart::{
    Annotation, AreaLayer, Axis, BarChart, BarGroup, BarMode, BarOrientation, BoxOrientation,
    BoxPlot, BubbleChart, BubbleSeries, Bullet, BulletItem, CalendarHeatmap, Candle, CandleStyle,
    CandlestickChart, ChartStack, DepthChart, Funnel, FunnelStage, Gantt, Gauge, GaugeOrientation,
    GaugeStyle, Gradient, Heatmap, Histogram, LabelPosition, LegendPosition, LineChart, LineMode,
    LollipopChart, LollipopItem, Marker, Overlay, PairedBars, PairedBarsItem, PairedBarsLayout,
    PieChart, PieSlice, RadarChart, RadarSeries, RadialBar, RadialRing, RadialSweep, RenkoChart,
    RenkoStyle, RoseChart, RoseItem, Sankey, SankeyLink, Series, SlopeChart, SlopeItem, Sparkline,
    SparklineStyle, StackDirection, StackedAreaChart, Stream, Task, Treemap, TreemapItem,
    ViolinChart, ViolinGroup, Waterfall, days_from_civil, sma, time_hms,
};

use super::{Rng, block};

pub(super) fn bars(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let groups = vec![
        BarGroup::new("Q1", [120.0, 80.0, 45.0]),
        BarGroup::new("Q2", [150.0, 95.0, 60.0]),
        BarGroup::new("Q3", [90.0, 110.0, 75.0]),
        BarGroup::new("Q4", [200.0, 130.0, 90.0]),
    ];
    BarChart::new(groups)
        .series_names(["North", "South", "West"])
        .colors(vec![Color::Cyan, Color::Yellow, Color::Magenta])
        .orientation(BarOrientation::Vertical)
        .mode(BarMode::Grouped)
        .show_values(true)
        .value_axis(Axis::new().grid(true).title("units"))
        .theme(&theme)
        .block(block(" Bar chart · Grouped "))
        .render(area, buf);
}

pub(super) fn boxplot(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(11);
    let data: Vec<Vec<f64>> = (0..4)
        .map(|group| {
            (0..80)
                .map(|_| 40.0 + group as f64 * 8.0 + rng.gauss() * (6.0 + group as f64 * 2.0))
                .collect()
        })
        .collect();
    let mut plot = BoxPlot::new()
        .orientation(BoxOrientation::Vertical)
        .show_outliers(true)
        .show_mean(true)
        .notched(false)
        .jitter(false)
        .theme(&theme)
        .block(block(" BoxPlot "));
    for (index, group) in data.iter().enumerate() {
        plot = plot.group(format!("G{}", index + 1), group.iter().copied());
    }
    plot.render(area, buf);
}

pub(super) fn bubble(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(5);
    let data: Vec<Vec<(f64, f64, f64)>> = (0..3)
        .map(|group| {
            (0..8)
                .map(|_| {
                    (
                        10.0 + group as f64 * 25.0 + rng.unit() * 30.0,
                        20.0 + group as f64 * 15.0 + rng.unit() * 40.0,
                        5.0 + rng.unit() * 95.0,
                    )
                })
                .collect()
        })
        .collect();
    let names = ["EU", "US", "APAC"];
    BubbleChart::new(
        data.iter()
            .zip(names)
            .map(|(points, name)| BubbleSeries::new(name, points.clone())),
    )
    .marker(Marker::Braille)
    .theme(&theme)
    .block(block(" Markets: spend vs revenue (size = users) "))
    .render(area, buf);
}

pub(super) fn bullet(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    Bullet::new([
        BulletItem::new("Revenue", 270.0)
            .target(300.0)
            .ranges([150.0, 250.0, 350.0]),
        BulletItem::new("Profit", 22.0)
            .target(27.0)
            .ranges([15.0, 25.0, 35.0]),
        BulletItem::new("Orders", 1500.0)
            .target(2000.0)
            .ranges([1000.0, 2000.0, 3000.0]),
        BulletItem::new("Satisfaction", 3.9)
            .target(4.5)
            .ranges([3.0, 4.0, 5.0]),
    ])
    .theme(&theme)
    .block(block(" KPIs "))
    .render(area, buf);
}

pub(super) fn calendar(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(3);
    let values: Vec<f64> = (0..182)
        .map(|day| {
            // 2024-07-01 is a Monday.
            let busy = if day % 7 < 5 { 1.0 } else { 0.25 };
            (rng.unit() * rng.unit() * 12.0 * busy).round()
        })
        .collect();
    CalendarHeatmap::new(days_from_civil(2024, 7, 1), values)
        .monday_first(true)
        .legend(true)
        .theme(&theme)
        .block(block(" Contributions · H2 2024 "))
        .render(area, buf);
}

fn next_candle(rng: &mut Rng, prev_close: f64, minute: usize) -> Candle {
    let open = prev_close;
    let close = open + rng.gauss() * 1.2;
    let high = open.max(close) + rng.unit() * 1.5;
    let low = open.min(close) - rng.unit() * 1.5;
    let volume = 500.0 + rng.unit() * 2500.0 + (close - open).abs() * 400.0;
    Candle::new(open, high, low, close)
        .volume(volume)
        .label(format!("{:02}:{:02}", 9 + minute / 60, minute % 60))
}

pub(super) fn candlestick(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(7);
    let mut candles = Vec::new();
    let mut price = 100.0;
    for minute in 0..200 {
        let candle = next_candle(&mut rng, price, minute);
        price = candle.close;
        candles.push(candle);
    }
    let last = candles.last().map(|candle| candle.close).unwrap_or(0.0);
    CandlestickChart::new(candles.clone())
        .style(CandleStyle::Candle)
        .show_volume(true)
        .scroll(0)
        .y_axis(Axis::new().grid(true).title("price"))
        .x_axis(Axis::new().ticks(6))
        .overlay(Overlay::new("SMA20", sma(&candles, 20), Color::Yellow))
        .theme(&theme)
        .block(Block::bordered().title(format!(" ACME · Candle · last {last:.2} ")))
        .render(area, buf);
}

pub(super) fn chartstack(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(11);
    let mut last = 100.0;
    let mut price = Vec::new();
    let mut trend = Vec::new();
    let mut volume = Vec::new();
    for index in 0..60 {
        last += rng.gauss() * 1.5;
        price.push((index as f64, last));
        trend.push((index as f64, 100.0 + index as f64 * 0.2));
        volume.push(10.0 + rng.unit() * 40.0);
    }
    let panes = ChartStack::new(StackDirection::Vertical)
        .weights([3, 2, 2])
        .split(area);
    let area_chart = LineChart::new(vec![
        Series::new("price").data(price.clone()).color(Color::Cyan),
    ])
    .mode(LineMode::Area)
    .y_axis(Axis::new().grid(true))
    .theme(&theme)
    .block(block(" Price "));
    area_chart.render(panes[0], buf);

    let trend_chart = LineChart::new(vec![
        Series::new("price").data(price).color(Color::Cyan),
        Series::new("trend").data(trend).color(Color::Yellow),
    ])
    .mode(LineMode::Step)
    .theme(&theme)
    .block(block(" Price vs trend "));
    trend_chart.render(panes[1], buf);

    let volumes: Vec<BarGroup> = volume
        .iter()
        .take(30)
        .map(|value| BarGroup::new("", [*value]))
        .collect();
    BarChart::new(volumes)
        .show_values(false)
        .label_position(LabelPosition::Outside)
        .colors(vec![Color::Magenta])
        .theme(&theme)
        .block(block(" Volume "))
        .render(panes[2], buf);
}

pub(super) fn depth(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mid = 100.0;
    let bids = (1..=40).map(|level| {
        (
            mid - 0.25 - level as f64 * 0.25,
            1.0 + (level as f64 * 1.7).sin().abs() * 5.0 + level as f64 * 0.2,
        )
    });
    let asks = (1..=40).map(|level| {
        (
            mid + 0.25 + level as f64 * 0.25,
            1.0 + (level as f64 * 1.3).cos().abs() * 5.0 + level as f64 * 0.2,
        )
    });
    DepthChart::new(bids, asks)
        .show_mid(true)
        .show_spread(true)
        .theme(&theme)
        .block(block(" Depth chart "))
        .render(area, buf);
}

pub(super) fn funnel(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    Funnel::new([
        FunnelStage::new("Visitors", 12_000.0),
        FunnelStage::new("Sign-ups", 4_800.0),
        FunnelStage::new("Trials", 2_100.0),
        FunnelStage::new("Paying", 780.0),
        FunnelStage::new("Retained", 520.0),
    ])
    .show_percent(true)
    .taper(true)
    .theme(&theme)
    .block(block(" Conversion funnel "))
    .render(area, buf);
}

pub(super) fn gantt(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let today: f64 = 12.0;
    let spec = [
        ("Research", 0.0, 8.0),
        ("Design", 6.0, 16.0),
        ("Backend", 10.0, 28.0),
        ("Frontend", 14.0, 30.0),
        ("Testing", 26.0, 36.0),
        ("Release", 36.0, 40.0),
    ];
    Gantt::new(spec.iter().map(|(name, start, end)| {
        let progress = ((today - start) / (end - start)).clamp(0.0, 1.0);
        Task::new(*name, *start, *end).progress(progress)
    }))
    .today(today)
    .theme(&theme)
    .block(block(" Gantt "))
    .render(area, buf);
}

pub(super) fn gauge(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let gauge = Gauge::new(55.0)
        .style(GaugeStyle::Linear)
        .orientation(GaugeOrientation::Horizontal)
        .marker(Marker::Braille)
        .bands([
            (50.0, Color::Green),
            (80.0, Color::Yellow),
            (100.0, Color::Red),
        ])
        .target(70.0)
        .label("55%")
        .theme(&theme)
        .block(block(" Gauge · Linear "));
    let height = area.height.min(5);
    let bar = Layout::vertical([Constraint::Length(height)])
        .flex(Flex::Center)
        .areas::<1>(area)[0];
    gauge.render(bar, buf);
}

pub(super) fn heatmap(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let grid: Vec<Vec<f64>> = (0..7)
        .map(|row| {
            (0..24)
                .map(|column| {
                    let x = column as f64 / 3.0;
                    let y = row as f64 / 1.5;
                    (x.sin() + y.cos()) * 50.0 + 50.0
                })
                .collect()
        })
        .collect();
    let hours: Vec<String> = (0..24).map(|hour| format!("{hour:02}")).collect();
    Heatmap::new(grid)
        .row_labels(["Mon", "Tue", "Wed", "Thu", "Fri", "Sat", "Sun"])
        .col_labels(hours)
        .stretch(true)
        .gradient(Gradient::Viridis)
        .show_values(false)
        .theme(&theme)
        .block(block(" Heatmap · Viridis "))
        .render(area, buf);
}

pub(super) fn histogram(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(3);
    let data: Vec<f64> = (0..2000).map(|_| 50.0 + rng.gauss() * 12.0).collect();
    Histogram::new(data)
        .bins(12)
        .kde(true)
        .stats(true)
        .orientation(BarOrientation::Vertical)
        .show_values(true)
        .gradient(Gradient::Viridis)
        .theme(&theme)
        .block(block(" Histogram · 12 bins · n=2000 "))
        .render(area, buf);
}

pub(super) fn line(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(42);
    let mut walk = vec![0.0];
    for _ in 0..119 {
        let last = *walk.last().unwrap_or(&0.0);
        walk.push(last + rng.gauss() * 0.3);
    }
    let sin = (0..120).map(|i| (i as f64, (i as f64 / 8.0).sin() * 2.0));
    let cos = (0..120).map(|i| (i as f64, (i as f64 / 12.0).cos() * 1.5));
    let walk = walk
        .iter()
        .enumerate()
        .map(|(index, value)| (index as f64, *value));
    LineChart::new(vec![
        Series::new("sin").data(sin).color(Color::Cyan),
        Series::new("cos").data(cos).color(Color::Yellow),
        Series::new("walk").data(walk).color(Color::Magenta),
    ])
    .mode(LineMode::Line)
    .marker(Marker::Braille)
    .legend(LegendPosition::TopRight)
    .x_axis(Axis::new().title("sample").grid(true))
    .y_axis(Axis::new().title("value").grid(true))
    .annotation(Annotation::y_region(1.0, 2.0).text("high"))
    .annotation(Annotation::hline(0.0).text("zero"))
    .annotation(Annotation::vline(60.0).text("mid"))
    .theme(&theme)
    .block(block(" Line chart · Line · Braille "))
    .render(area, buf);
}

const LOLLIPOP_DATA: [(&str, f64, f64); 8] = [
    ("Rust", 82.0, 61.0),
    ("Go", 64.0, 52.0),
    ("Zig", 38.0, 12.0),
    ("Python", 71.0, 74.0),
    ("Kotlin", 55.0, 40.0),
    ("Swift", 47.0, 43.0),
    ("Elixir", 29.0, 18.0),
    ("Haskell", 21.0, 24.0),
];

pub(super) fn lollipop(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    LollipopChart::new(
        LOLLIPOP_DATA
            .iter()
            .map(|(name, now, _before)| LollipopItem::new(*name, *now)),
    )
    .sorted(true)
    .show_values(true)
    .head('●')
    .theme(&theme)
    .block(block(" Language satisfaction (now vs last year) "))
    .render(area, buf);
}

pub(super) fn paired_bars(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    PairedBars::new([
        PairedBarsItem::new("mean ΔE", 3.2, 1.6),
        PairedBarsItem::new("p95 ΔE (worst 5%)", 8.4, 4.2),
        PairedBarsItem::new("over ΔE 7", 12.0, 6.0).unit("%"),
    ])
    .layout(PairedBarsLayout::Inline)
    .legend("46 shipped", "286 in use")
    .theme(&theme)
    .block(block(" Gain "))
    .render(area, buf);
}

const PIE_LANGUAGES: [(&str, Color); 5] = [
    ("Rust", Color::Rgb(222, 165, 132)),
    ("Go", Color::Rgb(0, 173, 216)),
    ("Python", Color::Rgb(255, 212, 59)),
    ("Zig", Color::Rgb(247, 164, 29)),
    ("Other", Color::Rgb(140, 140, 160)),
];

pub(super) fn pie(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(7);
    let values: Vec<f64> = PIE_LANGUAGES
        .iter()
        .map(|_| (5.0 + rng.unit() * 40.0).round())
        .collect();
    let slices: Vec<PieSlice> = PIE_LANGUAGES
        .iter()
        .zip(&values)
        .map(|((name, color), value)| PieSlice::new(name, *value, *color))
        .collect();
    PieChart::new(slices)
        .show_legend(true)
        .resolution(Resolution::Standard)
        .theme(&theme)
        .block(block(" Language share "))
        .render(area, buf);
}

pub(super) fn radar(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let a = (0..6).map(|i| 6.0 + 3.0 * (i as f64).sin());
    let b = (0..6).map(|i| 5.0 + 4.0 * (i as f64 * 2.0).cos());
    RadarChart::new(["Speed", "Power", "Range", "Armor", "Stealth", "Cost"])
        .series(RadarSeries::new("A", a).color(Color::Cyan))
        .series(RadarSeries::new("B", b).color(Color::Magenta))
        .max(10.0)
        .rings(4)
        .fill(true)
        .marker(Marker::Braille)
        .theme(&theme)
        .block(block(" Radar "))
        .render(area, buf);
}

fn wave(offset: f64) -> f64 {
    (50.0 + 45.0 * offset.sin()).clamp(0.0, 100.0)
}

pub(super) fn radial(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    RadialBar::new([
        RadialRing::new("Move", wave(0.0) * 6.0, 600.0),
        RadialRing::new("Exercise", wave(1.3) * 0.3, 30.0),
        RadialRing::new("Stand", wave(2.6) * 0.12, 12.0),
    ])
    .sweep(RadialSweep::Arc)
    .legend(true)
    .theme(&theme)
    .block(block(" Rings "))
    .render(area, buf);
}

pub(super) fn renko(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(11);
    let mut prices = vec![100.0];
    for _ in 0..300 {
        let last = *prices.last().unwrap_or(&100.0);
        prices.push(last + rng.gauss() * 0.8);
    }
    RenkoChart::new(&prices, 1.0)
        .style(RenkoStyle::Bricks)
        .theme(&theme)
        .block(block(" Renko · brick 1.0 · Bricks "))
        .render(area, buf);
}

pub(super) fn rose(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(17);
    let data: Vec<f64> = (0..12).map(|_| 5.0 + rng.unit() * 95.0).collect();
    let months = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    RoseChart::new(
        data.iter()
            .zip(months)
            .map(|(value, month)| RoseItem::new(month, value.round())),
    )
    .area_scale(true)
    .inner(0.0)
    .start_deg(0.0)
    .show_values(true)
    .theme(&theme)
    .block(block(" Monthly rainfall "))
    .render(area, buf);
}

pub(super) fn sankey(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(2);
    let scale = [
        0.5 + rng.unit(),
        0.5 + rng.unit(),
        0.5 + rng.unit(),
        0.5 + rng.unit(),
        0.5 + rng.unit(),
    ];
    let links = [
        SankeyLink::new("Salary", "Budget", (60.0 * scale[0]).round()),
        SankeyLink::new("Freelance", "Budget", (25.0 * scale[1]).round()),
        SankeyLink::new("Dividends", "Budget", (10.0 * scale[2]).round()),
        SankeyLink::new("Budget", "Housing", (35.0 * scale[3]).round()),
        SankeyLink::new("Budget", "Food", (18.0 * scale[4]).round()),
        SankeyLink::new("Budget", "Savings", (30.0 * scale[0]).round()),
        SankeyLink::new("Budget", "Fun", (12.0 * scale[1]).round()),
        SankeyLink::new("Savings", "ETF", (20.0 * scale[0]).round()),
        SankeyLink::new("Savings", "Cash", (10.0 * scale[2]).round()),
    ];
    Sankey::new(links)
        .show_values(true)
        .node_width(2)
        .theme(&theme)
        .block(block(" Money flow "))
        .render(area, buf);
}

const SLOPE_NAMES: [&str; 8] = [
    "Rust", "Go", "Zig", "Python", "Kotlin", "Swift", "Elixir", "Java",
];

pub(super) fn slope(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(21);
    let data: Vec<(&str, f64, f64)> = SLOPE_NAMES
        .iter()
        .map(|name| {
            let start = 10.0 + rng.unit() * 80.0;
            let end = (start + (rng.unit() - 0.45) * 50.0).clamp(2.0, 99.0);
            (*name, start.round(), end.round())
        })
        .collect();
    SlopeChart::new(
        data.iter()
            .map(|(name, start, end)| SlopeItem::new(*name, *start, *end)),
    )
    .titles("2022", "2025")
    .trend_colors(true)
    .theme(&theme)
    .block(block(" Popularity change "))
    .render(area, buf);
}

pub(super) fn sparkline(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(7);
    let mut data = vec![0.0];
    for _ in 0..239 {
        let last = *data.last().unwrap_or(&0.0);
        data.push((last + rng.gauss() * 1.4).clamp(-30.0, 30.0));
    }
    let [bar_area, area_area, line_area, loss_area] = Layout::vertical([
        Constraint::Fill(2),
        Constraint::Fill(3),
        Constraint::Fill(2),
        Constraint::Length(3),
    ])
    .areas(area);
    let spark = |style: SparklineStyle, title: &'static str| {
        Sparkline::new(data.iter().copied())
            .style(style)
            .marker(Marker::Braille)
            .smooth(1)
            .highlight_last(Color::White)
            .gradient([Color::Red, Color::Yellow, Color::Green])
            .theme(&theme)
            .block(block(title))
    };
    spark(SparklineStyle::Bar, " Bar ").render(bar_area, buf);
    spark(SparklineStyle::Area, " Area ").render(area_area, buf);
    spark(SparklineStyle::Line, " Line ").render(line_area, buf);
    let deltas: Vec<f64> = data.windows(2).map(|pair| pair[1] - pair[0]).collect();
    Sparkline::new(deltas)
        .style(SparklineStyle::WinLoss)
        .theme(&theme)
        .block(block(" Win / loss (tick-to-tick change) "))
        .render(loss_area, buf);
}

pub(super) fn stacked_area(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(4);
    let data: Vec<Vec<f64>> = (0..4)
        .map(|layer| {
            let phase = rng.unit() * 6.0;
            (0..48)
                .map(|i| {
                    20.0 + 12.0 * ((i as f64 / 5.0) + phase + layer as f64).sin() + rng.unit() * 4.0
                })
                .collect()
        })
        .collect();
    let names = ["web", "api", "batch", "cache"];
    StackedAreaChart::new(
        data.iter()
            .zip(names)
            .map(|(values, name)| AreaLayer::new(name, values.iter().copied())),
    )
    .normalize(false)
    .outline(true)
    .theme(&theme)
    .block(block(" Traffic by service "))
    .render(area, buf);
}

pub(super) fn stream(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(7);
    let mut price = Stream::new(120).x_start(1_700_000_000.0);
    price.push(100.0);
    let mut last = 100.0;
    let mut volume = Vec::new();
    for _ in 0..119 {
        last += rng.gauss() * 0.8;
        price.push(last);
        volume.push(10.0 + rng.unit() * 40.0);
    }
    let panes = ChartStack::vertical().weights([3, 1]).split(area);
    LineChart::new(vec![price.series("price").color(Color::Cyan)])
        .mode(LineMode::Area)
        .x_axis(Axis::new().formatter(time_hms).ticks(5))
        .y_axis(Axis::new().grid(true))
        .theme(&theme)
        .block(block(" Live price (UTC) "))
        .render(panes[0], buf);
    let volumes: Vec<BarGroup> = volume
        .iter()
        .rev()
        .take(24)
        .rev()
        .map(|value| BarGroup::new("", [*value]))
        .collect();
    BarChart::new(volumes)
        .show_values(false)
        .label_position(LabelPosition::Outside)
        .colors(vec![Color::Magenta])
        .theme(&theme)
        .block(block(" Volume "))
        .render(panes[1], buf);
}

const TREEMAP_NAMES: [&str; 8] = ["Rust", "Go", "Zig", "C", "C++", "Java", "Python", "Swift"];

pub(super) fn treemap(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(5);
    let weights: Vec<f64> = (0..TREEMAP_NAMES.len())
        .map(|_| 1.0 + rng.unit() * 30.0)
        .collect();
    let items = [
        TreemapItem::new("Systems", 0.0)
            .children((0..4).map(|i| TreemapItem::new(TREEMAP_NAMES[i], weights[i]))),
        TreemapItem::new("Apps", 0.0)
            .children((4..8).map(|i| TreemapItem::new(TREEMAP_NAMES[i], weights[i]))),
    ];
    Treemap::new(items)
        .border(false)
        .show_values(true)
        .theme(&theme)
        .block(block(" Treemap "))
        .render(area, buf);
}

pub(super) fn violin(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(8);
    let unimodal: Vec<f64> = (0..200).map(|_| 30.0 + rng.gauss() * 6.0).collect();
    let mut bimodal: Vec<f64> = (0..100).map(|_| 25.0 + rng.gauss() * 4.0).collect();
    bimodal.extend((0..100).map(|_| 55.0 + rng.gauss() * 5.0));
    let wide: Vec<f64> = (0..200).map(|_| 45.0 + rng.gauss() * 10.0).collect();
    ViolinChart::new([
        ViolinGroup::new("Unimodal", unimodal),
        ViolinGroup::new("Bimodal", bimodal),
        ViolinGroup::new("Wide", wide),
    ])
    .show_box(true)
    .theme(&theme)
    .block(block(" Latency distributions "))
    .render(area, buf);
}

pub(super) fn waterfall(area: Rect, buf: &mut Buffer) {
    let theme = preview_theme();
    let mut rng = Rng::new(5);
    let deltas: Vec<f64> = (0..6).map(|_| (rng.unit() - 0.35) * 100.0).collect();
    let mut chart = Waterfall::new().initial(100.0);
    for (index, delta) in deltas.iter().enumerate() {
        chart = chart.delta(format!("Q{}", index + 1), *delta);
    }
    chart
        .total("Total")
        .connectors(true)
        .show_values(true)
        .theme(&theme)
        .block(block(" Waterfall "))
        .render(area, buf);
}
