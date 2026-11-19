use std::sync::OnceLock;

use peri_genui::{Library, LibraryError};

// 位置参数由 JSON 属性顺序决定，不能经过会重排键的 Value 再编译。
const SCHEMA: &str = r##"{
  "root": "Stack",
  "schema": {
    "properties": {
      "Stack": {}, "Card": {}, "Text": {}, "Heading": {}, "List": {},
      "Table": {}, "KeyValue": {}, "Progress": {}, "Divider": {}, "Button": {},
      "BarChart": {}, "Sparkline": {}
    },
    "$defs": {
      "Stack": {"properties": {
        "children": {"type": "array", "items": {"anyOf": [
          {"$ref": "#/$defs/Stack"}, {"$ref": "#/$defs/Card"},
          {"$ref": "#/$defs/Text"}, {"$ref": "#/$defs/Heading"},
          {"$ref": "#/$defs/List"}, {"$ref": "#/$defs/Table"},
          {"$ref": "#/$defs/KeyValue"}, {"$ref": "#/$defs/Progress"},
          {"$ref": "#/$defs/Divider"}, {"$ref": "#/$defs/Button"},
          {"$ref": "#/$defs/BarChart"}, {"$ref": "#/$defs/Sparkline"}
        ]}},
        "direction": {"type": "string", "enum": ["column", "row"]}
      }, "required": ["children"]},
      "Card": {"properties": {
        "children": {"type": "array", "items": {"anyOf": [
          {"$ref": "#/$defs/Stack"}, {"$ref": "#/$defs/Card"},
          {"$ref": "#/$defs/Text"}, {"$ref": "#/$defs/Heading"},
          {"$ref": "#/$defs/List"}, {"$ref": "#/$defs/Table"},
          {"$ref": "#/$defs/KeyValue"}, {"$ref": "#/$defs/Progress"},
          {"$ref": "#/$defs/Divider"}, {"$ref": "#/$defs/Button"},
          {"$ref": "#/$defs/BarChart"}, {"$ref": "#/$defs/Sparkline"}
        ]}},
        "title": {"type": "string"}
      }, "required": ["children"]},
      "Text": {"properties": {
        "content": {"type": "string"}, "variant": {"type": "string"}
      }, "required": ["content"]},
      "Heading": {"properties": {
        "text": {"type": "string"}, "level": {"type": "integer"}
      }, "required": ["text"]},
      "List": {"properties": {
        "items": {"type": "array", "items": {"type": "string"}},
        "ordered": {"type": "boolean"}
      }, "required": ["items"]},
      "Table": {"properties": {
        "columns": {"type": "array", "items": {"type": "string"}},
        "rows": {"type": "array", "items": {"type": "array"}},
        "title": {"type": "string"}
      }, "required": ["columns", "rows"]},
      "KeyValue": {"properties": {
        "items": {"type": "array", "items": {"type": "object", "properties": {
          "key": {"type": "string"}, "value": {}
        }, "required": ["key", "value"]}},
        "title": {"type": "string"}
      }, "required": ["items"]},
      "Progress": {"properties": {
        "label": {"type": "string"}, "value": {"type": "number"},
        "max": {"type": "number"}
      }, "required": ["value"]},
      "Divider": {"properties": {}},
      "Button": {"properties": {
        "label": {"type": "string"}, "action": {}, "variant": {"type": "string"}
      }, "required": ["label", "action"]},
      "BarChart": {"properties": {
        "items": {"type": "array", "items": {"type": "object", "properties": {
          "label": {"type": "string"}, "value": {"type": "number"}
        }, "required": ["label", "value"]}},
        "title": {"type": "string"}, "max": {"type": "number"}
      }, "required": ["items"]},
      "Sparkline": {"properties": {
        "values": {"type": "array", "items": {"type": "number"}},
        "label": {"type": "string"}
      }, "required": ["values"]}
    }
  }
}"##;

pub fn library() -> Result<&'static Library, &'static LibraryError> {
    static LIBRARY: OnceLock<Result<Library, LibraryError>> = OnceLock::new();
    LIBRARY
        .get_or_init(|| Library::from_json_str(SCHEMA))
        .as_ref()
}
