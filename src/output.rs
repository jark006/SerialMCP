//! Bounded tool output. Compact responses have exactly one representation.
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::VecDeque,
    sync::{
        Mutex, OnceLock,
        atomic::{AtomicU64, Ordering},
    },
};

pub const DEFAULT_OUTPUT_LIMIT: usize = 8192;
const CACHE_BYTES: usize = 16 * 1024 * 1024;
const CACHE_ENTRIES: usize = 16;

#[derive(Clone, Copy, Default, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResponseMode {
    #[default]
    Compact,
    Detailed,
}

struct Entry {
    id: String,
    value: Value,
    size: usize,
}
static CACHE: OnceLock<Mutex<VecDeque<Entry>>> = OnceLock::new();
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

fn retain(value: Value) -> Option<String> {
    let size = value.to_string().len();
    if size > CACHE_BYTES {
        return None;
    }
    let mut cache = CACHE.get_or_init(Default::default).lock().ok()?;
    while cache.len() >= CACHE_ENTRIES
        || cache.iter().map(|e| e.size).sum::<usize>() + size > CACHE_BYTES
    {
        cache.pop_front();
    }
    let id = format!("output-{}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    cache.push_back(Entry {
        id: id.clone(),
        value,
        size,
    });
    Some(id)
}

fn boundary(text: &str, limit: usize) -> usize {
    let mut end = limit.min(text.len());
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    end
}

pub fn success(_name: &str, value: Value, mode: ResponseMode, limit: usize) -> Value {
    // MCP structuredContent must be an object, including for port enumeration.
    let mut value = if value.is_array() {
        json!({"ports": value})
    } else {
        value
    };
    let original = value.clone();
    if matches!(mode, ResponseMode::Compact) {
        if let Some(object) = value.as_object_mut() {
            object.remove("hex");
            object.remove("raw_data");
        }
        remove_nulls(&mut value);
    }
    let mut truncated = false;
    for key in ["data", "raw_data", "hex"] {
        if let Some(text) = value.get(key).and_then(Value::as_str)
            && text.len() > limit
        {
            let end = boundary(text, limit);
            value[key] = json!(&text[..end]);
            truncated = true;
        }
    }
    if truncated {
        value["truncated"] = json!(true);
        if let Some(id) = retain(original) {
            value["output_id"] = json!(id);
            value["output_note"] = json!(
                "Use serial_get_output to page retained data, raw_data, or hex. Cache: last 16 outputs, 16 MiB total; cleared on restart."
            );
        } else {
            value["output_note"] = json!(
                "Output could not be retained; request a larger output_limit on the original operation."
            );
        }
    }
    let failed = value.get("error").is_some_and(|error| !error.is_null());
    let text = value.to_string();
    let mut result = json!({"content": [{"type": "text", "text": text}], "isError": failed});
    if matches!(mode, ResponseMode::Detailed) {
        result["structuredContent"] = value;
    }
    result
}

fn remove_nulls(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.retain(|_, value| !value.is_null());
            for value in object.values_mut() {
                remove_nulls(value);
            }
        }
        Value::Array(items) => items.iter_mut().for_each(remove_nulls),
        _ => {}
    }
}

pub fn page(id: &str, field: &str, offset: usize, limit: usize) -> Result<Value, String> {
    let cache = CACHE
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| "output cache lock poisoned")?;
    let entry = cache
        .iter()
        .find(|e| e.id == id)
        .ok_or("output expired or unknown; cache is bounded and cleared on restart")?;
    let text = entry
        .value
        .get(field)
        .and_then(Value::as_str)
        .ok_or("requested field is not available in this output")?;
    if offset > text.len() || !text.is_char_boundary(offset) {
        return Err("offset must be a UTF-8 byte boundary within the output".into());
    }
    let end = offset + boundary(&text[offset..], limit);
    Ok(
        json!({"output_id": id, "field": field, "offset": offset, "next_offset": end,
        "total_bytes": text.len(), "has_more": end < text.len(), "data": &text[offset..end]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn compact_is_single_copy_and_preserves_completion_metadata() {
        let value = json!({"data":"ok", "hex":"6F 6B", "raw_data":"ok", "stop_reason":"timeout", "matched":false});
        let compact = success("serial_read", value.clone(), ResponseMode::Compact, 8192);
        let detailed = success("serial_read", value, ResponseMode::Detailed, 8192);
        assert!(compact.get("structuredContent").is_none());
        let text: Value =
            serde_json::from_str(compact["content"][0]["text"].as_str().unwrap()).unwrap();
        assert_eq!(text["stop_reason"], "timeout");
        assert!(text.get("hex").is_none());
        assert!(compact.to_string().len() < detailed.to_string().len());
    }
    #[test]
    fn truncated_unicode_and_binary_remain_retrievable() {
        let result = success(
            "serial_read",
            json!({"data":"中文abc", "hex":"E4 B8 AD E6 96 87 61 62 63"}),
            ResponseMode::Detailed,
            4,
        );
        let value = &result["structuredContent"];
        assert_eq!(value["data"], "中");
        let id = value["output_id"].as_str().unwrap();
        assert_eq!(page(id, "data", 3, 6).unwrap()["data"], "文abc");
        assert!(page(id, "data", 1, 6).is_err());
        assert_eq!(
            page(id, "hex", 0, 100).unwrap()["data"],
            "E4 B8 AD E6 96 87 61 62 63"
        );
    }
}
