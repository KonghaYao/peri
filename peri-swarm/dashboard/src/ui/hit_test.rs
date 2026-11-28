use ratatui::layout::Rect;

use super::*;
use crate::app::Page;

fn rect(x: u16, y: u16, width: u16, height: u16) -> Rect {
    Rect::new(x, y, width, height)
}

#[test]
fn clicks_resolve_inside_and_miss_outside() {
    let mut hits = HitMap::default();
    hits.region(
        Layer::Base,
        rect(2, 2, 4, 2),
        Intent::SelectPage(Page::Agents),
    );

    assert_eq!(
        hits.click(false, 2, 2),
        Some(Intent::SelectPage(Page::Agents))
    );
    assert_eq!(
        hits.click(false, 5, 3),
        Some(Intent::SelectPage(Page::Agents))
    );
    assert_eq!(hits.click(false, 6, 3), None);
    assert_eq!(hits.click(false, 2, 4), None);
}

#[test]
fn overlay_regions_shadow_the_base_layer() {
    let mut hits = HitMap::default();
    hits.region(
        Layer::Base,
        rect(0, 0, 10, 10),
        Intent::SelectPage(Page::Agents),
    );
    hits.region(Layer::Overlay, rect(2, 2, 4, 4), Intent::CloseOverlay);

    // Without a modal, the overlay region is inert.
    assert_eq!(
        hits.click(false, 3, 3),
        Some(Intent::SelectPage(Page::Agents))
    );
    // With a modal, base clicks are impossible.
    assert_eq!(hits.click(true, 3, 3), Some(Intent::CloseOverlay));
    assert_eq!(hits.click(true, 0, 0), None);
}

#[test]
fn later_regions_win() {
    let mut hits = HitMap::default();
    hits.region(Layer::Base, rect(0, 0, 10, 1), Intent::Retry);
    hits.region(Layer::Base, rect(4, 0, 2, 1), Intent::OpenSettings);

    assert_eq!(hits.click(false, 4, 0), Some(Intent::OpenSettings));
    assert_eq!(hits.click(false, 1, 0), Some(Intent::Retry));
}

#[test]
fn translated_regions_are_clipped_to_the_body() {
    let mut hits = HitMap::default();
    hits.region(Layer::Base, rect(0, 0, 4, 2), Intent::Retry);
    hits.region(Layer::Base, rect(0, 4, 4, 2), Intent::OpenSettings);
    let start = 0;
    let body = rect(10, 5, 20, 6);
    // Scroll 2 rows: first region starts above the body, second is fully in.
    hits.translate_since(start, body.x, i32::from(body.y) - 2, body);

    assert_eq!(hits.region_rect(Intent::Retry), None);
    let settings = hits
        .region_rect(Intent::OpenSettings)
        .expect("visible part");
    assert_eq!(settings, rect(10, 7, 4, 2));
    assert!(hits.click(false, 10, 7).is_some());
    assert!(hits.click(false, 10, 9).is_none());
}

#[test]
fn overlay_area_reports_backdrop_clicks() {
    let mut hits = HitMap::default();
    hits.set_overlay_area(rect(5, 5, 10, 4));

    assert!(!hits.outside_overlay(5, 5));
    assert!(!hits.outside_overlay(14, 8));
    assert!(hits.outside_overlay(4, 5));
    assert!(hits.outside_overlay(15, 8));
}

#[test]
fn wheel_zones_respect_modals() {
    let mut hits = HitMap::default();
    hits.scroll_zone(rect(0, 0, 10, 10), ScrollZone::Content);

    assert_eq!(hits.wheel_zone(false, 1, 1), Some(ScrollZone::Content));
    assert_eq!(hits.wheel_zone(true, 1, 1), None);
    assert_eq!(hits.wheel_zone(false, 11, 1), None);
    assert!(hits.overlay_area().is_none());
}
