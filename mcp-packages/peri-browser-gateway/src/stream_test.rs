use super::*;

#[test]
fn frame_metadata_names_the_view_which_produced_the_binary() {
    let frame = frame("p_selected", "4:2", b"latest");
    assert_eq!(
        serde_json::to_value(frame.meta_message()).unwrap(),
        json!({
            "t": "meta", "pageId": "p_selected", "viewEpoch": "4:2",
            "viewportWidth": 640, "viewportHeight": 480, "deviceWidth": 1280, "deviceHeight": 960,
            "pageScaleFactor": 2.0, "scrollOffsetX": 0.0, "scrollOffsetY": 12.0,
        })
    );
    assert_eq!(frame.jpeg.as_ref(), b"latest");
}

#[tokio::test]
async fn a_lagging_viewer_observes_the_latest_frame_and_cache_invalidation() {
    let streams = Streams::new();
    let mut viewer = streams.frame.subscribe();
    streams
        .frame
        .send_replace(Some(Arc::new(frame("p_old", "1:0", b"old"))));
    streams
        .frame
        .send_replace(Some(Arc::new(frame("p_new", "2:0", b"new"))));
    viewer.changed().await.unwrap();
    let latest = viewer.borrow_and_update().clone().unwrap();
    assert_eq!(latest.stamp.page_id.as_deref(), Some("p_new"));
    assert_eq!(latest.jpeg.as_ref(), b"new");
    assert!(!viewer.has_changed().unwrap());
    streams.frame.send_replace(None);
    viewer.changed().await.unwrap();
    assert!(viewer.borrow_and_update().is_none());
}

#[test]
fn thumbnail_mime_follows_the_magic_bytes() {
    assert_eq!(thumbnail_mime("iVBORw0KGgo="), "image/png");
    assert_eq!(thumbnail_mime("/9j/4AAQ"), "image/jpeg");
    assert_eq!(thumbnail_mime("bad"), "image/jpeg");
}

fn frame(id: &str, epoch: &str, bytes: &'static [u8]) -> Frame {
    Frame {
        jpeg: Bytes::from_static(bytes),
        stamp: ViewStamp {
            page_id: Some(id.to_owned()),
            view_epoch: epoch.to_owned(),
        },
        meta: FrameMeta {
            viewport_width: 640,
            viewport_height: 480,
            device_width: 1280,
            device_height: 960,
            page_scale_factor: 2.0,
            scroll_offset_x: 0.0,
            scroll_offset_y: 12.0,
        },
    }
}
