//! Bounded views of a complete derived assertion document. References never discard data.
use crate::asserted_query::AGENT_COMMAND;
use serde_json::{Value, json};
use supercov_engine::agent_json;

const INLINE_BYTES: usize = 8_192;
const MAX_PAGE_ITEMS: usize = 1_000;

fn kind(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

pub fn reference(value: &Value, pointer: &str) -> Value {
    json!({"pointer":pointer,"kind":kind(value),"bytes":serde_json::to_vec(value).unwrap().len(),
        "count":match value { Value::Array(a) => Some(a.len()), Value::Object(o) => Some(o.len()), _ => None }})
}

pub fn record(value: &Value, pointer: &str) -> Value {
    let evidence = reference(value, pointer);
    if evidence["bytes"].as_u64().unwrap() > INLINE_BYTES as u64 {
        return json!({"detailOnly":true,"evidence":evidence});
    }
    let mut row = value.clone();
    row["evidence"] = evidence;
    row
}

pub fn page(
    mut base: Value,
    collection: &str,
    rows: &[Value],
    offset: usize,
    limit: usize,
) -> Result<Value, String> {
    let selected = rows
        .iter()
        .skip(offset)
        .take(limit.min(MAX_PAGE_ITEMS))
        .cloned()
        .collect::<Vec<_>>();
    let mut render = |returned: usize| {
        let next = offset.saturating_add(returned);
        let more = returned > 0 && next < rows.len();
        base["pagination"] = json!({"offset":offset,"limit":limit,"total":rows.len(),"returned":returned,
            "hasMore":more,"nextOffset":more.then_some(next),"unit":"items",
            "byteLimited": returned < selected.len(),
            "itemLimited": selected.len() < limit.min(rows.len().saturating_sub(offset))});
        base[collection] = json!(&selected[..returned]);
        agent_json::success(AGENT_COMMAND, &base, None).is_ok()
    };
    // Find a fitting prefix without serializing every successively shorter page.
    let mut low = 0;
    let mut high = selected.len();
    if !render(high) {
        if !render(0) {
            return Err("Evidence reference metadata exceeds the response budget".into());
        }
        while low < high {
            let middle = low + (high - low).div_ceil(2);
            if render(middle) {
                low = middle;
            } else {
                high = middle - 1;
            }
        }
    } else {
        low = high;
    }
    render(low);
    if low == 0 && offset < rows.len() {
        return Err(
            "A single evidence reference exceeds the response budget; no evidence was discarded"
                .into(),
        );
    }
    Ok(base)
}

fn escape(segment: &str) -> String {
    segment.replace('~', "~0").replace('/', "~1")
}

pub fn evidence(
    base: Value,
    document: &Value,
    pointer: &str,
    offset: usize,
    limit: usize,
) -> Result<Value, String> {
    if !pointer.is_empty() && !pointer.starts_with('/') {
        return Err(
            "Evidence path must be a JSON pointer starting with '/' (or empty for the root)".into(),
        );
    }
    for segment in pointer.split('/') {
        let mut chars = segment.chars();
        while let Some(c) = chars.next() {
            if c == '~' && !matches!(chars.next(), Some('0' | '1')) {
                return Err("Invalid JSON pointer escape; use ~0 for '~' and ~1 for '/'".into());
            }
        }
    }
    let value = document
        .pointer(pointer)
        .ok_or("Evidence pointer does not exist in this analysis")?;
    let mut base = base;
    base["view"] = json!("evidence");
    base["path"] = json!(pointer);
    base["kind"] = json!(kind(value));
    if let Value::String(text) = value {
        // At most 4,000 Unicode scalar values, including worst-case JSON escaping.
        // Never split a UTF-8 sequence, and expose the next offset explicitly.
        let part = text
            .chars()
            .skip(offset)
            .take(limit.min(4_000))
            .collect::<String>();
        let returned = part.chars().count();
        let total = text.chars().count();
        let next = offset.saturating_add(returned);
        let more = returned > 0 && next < total;
        base["text"] = json!(part);
        base["pagination"] = json!({"offset":offset,"limit":limit,"total":total,"returned":returned,
            "hasMore":more,"nextOffset":more.then_some(next),"unit":"unicodeScalars"});
        return Ok(base);
    }
    let entry = |child: &Value, pointer: String, key: Value| {
        let mut entry = reference(child, &pointer);
        entry["key"] = key;
        if entry["bytes"].as_u64().unwrap() <= INLINE_BYTES as u64 {
            entry["value"] = child.clone();
        } else {
            entry["detailOnly"] = json!(true);
        }
        entry
    };
    let rows = match value {
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| entry(v, format!("{pointer}/{i}"), json!(i)))
            .collect(),
        Value::Object(o) => o
            .iter()
            .map(|(k, v)| entry(v, format!("{pointer}/{}", escape(k)), json!(k)))
            .collect(),
        _ => vec![entry(value, pointer.into(), Value::Null)],
    };
    page(base, "items", &rows, offset, limit)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn big_shared_evidence_and_single_records_remain_completely_addressable() {
        let text = "\0😀".repeat(30_000);
        let document = json!({"tests":[{"observations":[{"message":text}]}]});
        let root = evidence(json!({}), &document, "/tests", 0, 20).unwrap();
        assert_eq!(root["items"][0]["detailOnly"], true);
        assert_eq!(root["items"][0]["pointer"], "/tests/0");
        let mut joined = String::new();
        let mut offset = 0;
        loop {
            let part = evidence(
                json!({}),
                &document,
                "/tests/0/observations/0/message",
                offset,
                usize::MAX,
            )
            .unwrap();
            assert!(agent_json::success(AGENT_COMMAND, &part, None).is_ok());
            joined.push_str(part["text"].as_str().unwrap());
            let Some(next) = part["pagination"]["nextOffset"].as_u64() else {
                break;
            };
            assert!(next as usize > offset);
            offset = next as usize;
        }
        assert_eq!(joined, text);
    }

    #[test]
    fn adaptive_pages_have_no_omissions_or_duplicates_and_honor_end_offsets() {
        let values = (0..50)
            .map(|i| json!({"id":i,"text":"x".repeat(7_000)}))
            .collect::<Vec<_>>();
        let mut gathered = vec![];
        let mut offset = 0;
        loop {
            let p = page(json!({}), "sites", &values, offset, 50).unwrap();
            assert!(agent_json::success(AGENT_COMMAND, &p, None).is_ok());
            gathered.extend(p["sites"].as_array().unwrap().iter().cloned());
            let Some(next) = p["pagination"]["nextOffset"].as_u64() else {
                break;
            };
            offset = next as usize;
        }
        assert_eq!(gathered, values);
        assert_eq!(
            page(json!({}), "sites", &values, 500, 20).unwrap()["pagination"]["returned"],
            0
        );
        assert_eq!(
            record(&json!({"text":"x".repeat(100_000)}), "/sites/0")["detailOnly"],
            true
        );
        let capped = page(json!({}), "items", &vec![Value::Null; 2_000], 0, 2_000).unwrap();
        assert_eq!(capped["pagination"]["returned"], 1_000);
        assert_eq!(capped["pagination"]["byteLimited"], false);
        assert_eq!(capped["pagination"]["itemLimited"], true);
        assert!(page(json!({}), "items", &[json!("x".repeat(100_000))], 0, 1).is_err());
        assert!(page(json!({"meta":"x".repeat(100_000)}), "items", &[], 0, 1).is_err());
    }

    #[test]
    fn pointer_escaping_is_unambiguous_and_bad_paths_fail() {
        let d = json!({"a/b":{"~c":7}});
        assert_eq!(
            evidence(json!({}), &d, "/a~1b/~0c", 0, 1).unwrap()["items"][0]["value"],
            7
        );
        for bad in ["missing", "/a~2b", "/a~", "/nope"] {
            assert!(evidence(json!({}), &d, bad, 0, 1).is_err(), "{bad}");
        }
    }
}
