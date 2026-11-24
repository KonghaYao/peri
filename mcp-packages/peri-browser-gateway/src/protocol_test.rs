use serde_json::json;

use super::*;

#[test]
fn control_commands_require_id_and_current_view_even_when_no_tab_exists() {
    let input = json!({"t": "newTab", "id": 3, "view": {"pageId": null, "viewEpoch": "0:0"}, "url": "about:blank"});
    let command: Command = serde_json::from_value(input.clone()).unwrap();
    assert_eq!(command.id, 3);
    assert!(command.view.page_id.is_none());
    assert_eq!(command.view.view_epoch, "0:0");
    assert!(matches!(command.action, Action::NewTab { url } if url == "about:blank"));
    for key in ["id", "view", "url"] {
        let mut incomplete = input.clone();
        incomplete.as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<Command>(incomplete).is_err());
    }
    for key in ["pageId", "viewEpoch"] {
        let mut incomplete = input.clone();
        incomplete["view"].as_object_mut().unwrap().remove(key);
        assert!(serde_json::from_value::<Command>(incomplete).is_err());
    }
}

#[test]
fn switches_answers_done_and_inputs_keep_their_wire_fields() {
    let view = json!({"pageId": "p_old", "viewEpoch": "2:1"});
    let decode = |mut body: Value| {
        body["view"] = view.clone();
        body["id"] = json!(8);
        serde_json::from_value::<Command>(body).unwrap().action
    };
    assert!(
        matches!(decode(json!({"t": "switch", "pageId": "p_next"})), Action::Switch { page_id } if page_id == "p_next")
    );
    assert!(
        matches!(decode(json!({"t": "answer", "requestId": "handoff-id", "accepted": true})), Action::Answer { request_id, accepted: true } if request_id == "handoff-id")
    );
    assert!(
        matches!(decode(json!({"t": "done", "requestId": "handoff-id"})), Action::Done { request_id } if request_id == "handoff-id")
    );
    assert!(matches!(
        decode(
            json!({"t": "mouse", "type": "down", "x": 15, "y": 50, "button": "left", "clickCount": 2})
        ),
        Action::Mouse {
            event: MouseEventKind::Down,
            x: 15.0,
            y: 50.0,
            button: Some(MouseButton::Left),
            click_count: Some(2)
        }
    ));
    assert!(matches!(
        decode(json!({"t": "wheel", "x": 4, "y": 5, "deltaX": 1, "deltaY": -6})),
        Action::Wheel {
            x: 4.0,
            y: 5.0,
            delta_x: 1.0,
            delta_y: -6.0
        }
    ));
}

#[test]
fn only_ack_and_visibility_are_unstamped_and_old_dto_tags_cannot_act() {
    assert!(matches!(
        serde_json::from_value::<Signal>(json!({"t": "ack"})).unwrap(),
        Signal::Ack
    ));
    assert!(matches!(
        serde_json::from_value::<Signal>(json!({"t": "visible", "visible": false})).unwrap(),
        Signal::Visible { visible: false }
    ));
    for tag in ["yield_ok", "reclaim", "register", "unknown", "takeover"] {
        assert!(serde_json::from_value::<Signal>(json!({"t": tag})).is_err());
        assert!(serde_json::from_value::<Command>(json!({"t": tag})).is_err());
    }
}

#[test]
fn correlated_results_and_thumbnails_have_the_expected_wire_shape() {
    assert_eq!(
        serde_json::to_value(ServerMessage::Result {
            id: 17,
            ok: false,
            value: Value::Null,
            code: Some("read_only")
        })
        .unwrap(),
        json!({"t": "result", "id": 17, "ok": false, "value": null, "code": "read_only"})
    );
    assert_eq!(
        serde_json::to_value(ServerMessage::Result {
            id: 18,
            ok: true,
            value: json!({"pageId": "p_owned"}),
            code: None
        })
        .unwrap(),
        json!({"t": "result", "id": 18, "ok": true, "value": {"pageId": "p_owned"}, "code": null})
    );
    assert_eq!(
        serde_json::to_value(ServerMessage::Thumb {
            page_id: "p_owned",
            data: "data:image/jpeg;base64,/9g="
        })
        .unwrap(),
        json!({"t": "thumb", "pageId": "p_owned", "data": "data:image/jpeg;base64,/9g="})
    );
}
