//! JSONC parsing and structural text reconciliation.

use std::error::Error;
use std::fmt;

use serde::de::{self, Deserialize, Deserializer, MapAccess, SeqAccess, Visitor};
use serde_json::{Map, Value};

/// A malformed JSONC document or an edit that cannot converge safely.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JsoncError(String);

impl JsoncError {
    fn new(message: impl Into<String>) -> Self {
        Self(message.into())
    }
}

impl fmt::Display for JsoncError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for JsoncError {}

/// Parse JSON with line comments, block comments, and trailing commas.
///
/// # Errors
///
/// Returns [`JsoncError`] when the resulting JSON document is invalid.
pub fn parse(text: &str) -> Result<Value, JsoncError> {
    let blanked = blank_trailing_commas(&blank_comments(text));
    parse_unique(&blanked)
}

/// Parse strict JSON while rejecting duplicate object keys at every depth.
///
/// # Errors
///
/// Returns [`JsoncError`] when the document is invalid or ambiguous.
pub fn parse_json(text: &str) -> Result<Value, JsoncError> {
    parse_unique(text)
}

/// Reconcile a JSONC object with a semantic value while preserving untouched
/// source ranges.
///
/// # Errors
///
/// Returns [`JsoncError`] when the source is invalid or a safe splice cannot
/// be found.
pub fn merge(source: &str, desired: &Value) -> Result<String, JsoncError> {
    let mut current = source.to_owned();
    for _ in 0..4_096 {
        let parsed = parse(&current)?;
        if parsed == *desired {
            return Ok(current);
        }
        let edit = next_edit(&current, &parsed, desired, &[])?
            .ok_or_else(|| JsoncError::new("JSONC edit made no semantic progress"))?;
        current.replace_range(edit.start..edit.end, &edit.replacement);
    }
    Err(JsoncError::new("JSONC edit did not converge"))
}

/// Replace comments with equal-width spaces while retaining newlines.
#[must_use]
pub fn blank_comments(text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    let mut line_comment = false;
    let mut block_comment = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if line_comment {
            if byte == b'\n' {
                line_comment = false;
            } else {
                bytes[index] = b' ';
            }
        } else if block_comment {
            if byte == b'*' && bytes.get(index + 1) == Some(&b'/') {
                bytes[index] = b' ';
                bytes[index + 1] = b' ';
                index += 1;
                block_comment = false;
            } else if byte != b'\n' && byte != b'\r' {
                bytes[index] = b' ';
            }
        } else if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
        } else if byte == b'"' {
            in_string = true;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'/') {
            bytes[index] = b' ';
            bytes[index + 1] = b' ';
            index += 1;
            line_comment = true;
        } else if byte == b'/' && bytes.get(index + 1) == Some(&b'*') {
            bytes[index] = b' ';
            bytes[index + 1] = b' ';
            index += 1;
            block_comment = true;
        }
        index += 1;
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

fn blank_trailing_commas(text: &str) -> String {
    let mut bytes = text.as_bytes().to_vec();
    let mut in_string = false;
    let mut escaped = false;
    for index in 0..bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            continue;
        }
        if byte == b'"' {
            in_string = true;
            continue;
        }
        if byte != b',' {
            continue;
        }
        let next = skip_space(&bytes, index + 1);
        if matches!(bytes.get(next), Some(b'}' | b']')) {
            bytes[index] = b' ';
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

struct UniqueValue(Value);

impl<'de> Deserialize<'de> for UniqueValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        deserializer.deserialize_any(UniqueVisitor)
    }
}

struct UniqueVisitor;

impl<'de> Visitor<'de> for UniqueVisitor {
    type Value = UniqueValue;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a JSON value without duplicate object keys")
    }

    fn visit_bool<E>(self, value: bool) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Bool(value)))
    }

    fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }

    fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Number(value.into())))
    }

    fn visit_f64<E>(self, value: f64) -> Result<Self::Value, E>
    where
        E: de::Error,
    {
        serde_json::Number::from_f64(value)
            .map(Value::Number)
            .map(UniqueValue)
            .ok_or_else(|| E::custom("JSON number must be finite"))
    }

    fn visit_str<E>(self, value: &str) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value.to_owned())))
    }

    fn visit_string<E>(self, value: String) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::String(value)))
    }

    fn visit_none<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_unit<E>(self) -> Result<Self::Value, E> {
        Ok(UniqueValue(Value::Null))
    }

    fn visit_seq<A>(self, mut sequence: A) -> Result<Self::Value, A::Error>
    where
        A: SeqAccess<'de>,
    {
        let mut values = Vec::new();
        while let Some(value) = sequence.next_element::<UniqueValue>()? {
            values.push(value.0);
        }
        Ok(UniqueValue(Value::Array(values)))
    }

    fn visit_map<A>(self, mut object: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        let mut values = Map::new();
        while let Some(key) = object.next_key::<String>()? {
            if values.contains_key(&key) {
                return Err(de::Error::custom(format!(
                    "duplicate JSON object key {key:?}"
                )));
            }
            let value = object.next_value::<UniqueValue>()?;
            values.insert(key, value.0);
        }
        Ok(UniqueValue(Value::Object(values)))
    }
}

fn parse_unique(text: &str) -> Result<Value, JsoncError> {
    serde_json::from_str::<UniqueValue>(text)
        .map(|value| value.0)
        .map_err(|error| JsoncError::new(error.to_string()))
}

#[derive(Debug)]
struct Edit {
    start: usize,
    end: usize,
    replacement: String,
}

#[derive(Clone, Debug)]
struct Member {
    key: String,
    key_start: usize,
    value_start: usize,
    value_end: usize,
    comma: Option<usize>,
}

fn next_edit(
    source: &str,
    current: &Value,
    desired: &Value,
    path: &[String],
) -> Result<Option<Edit>, JsoncError> {
    if current == desired {
        return Ok(None);
    }
    let (Some(current_object), Some(desired_object)) = (current.as_object(), desired.as_object())
    else {
        let span = value_span(source, path)?;
        return Ok(Some(Edit {
            start: span.0,
            end: span.1,
            replacement: render_value(desired, indentation(source, span.0))?,
        }));
    };

    let span = object_span(source, path)?;
    let members = object_members(source, span.0)?;
    for (key, desired_value) in desired_object {
        match current_object.get(key) {
            Some(current_value) if current_value == desired_value => {}
            Some(current_value) if current_value.is_object() && desired_value.is_object() => {
                let mut child = path.to_vec();
                child.push(key.clone());
                if let Some(edit) = next_edit(source, current_value, desired_value, &child)? {
                    return Ok(Some(edit));
                }
            }
            Some(_) => {
                let member = members
                    .iter()
                    .find(|member| member.key == *key)
                    .ok_or_else(|| {
                        JsoncError::new(format!("cannot locate JSONC member {key:?}"))
                    })?;
                return Ok(Some(Edit {
                    start: member.value_start,
                    end: member.value_end,
                    replacement: render_value(
                        desired_value,
                        indentation(source, member.value_start),
                    )?,
                }));
            }
            None => return Ok(Some(insertion(source, span, &members, key, desired_value)?)),
        }
    }

    for member in &members {
        if !desired_object.contains_key(&member.key) {
            return Ok(Some(removal(source, &members, member)));
        }
    }
    Err(JsoncError::new(
        "JSONC objects differ without an editable member",
    ))
}

fn object_span(source: &str, path: &[String]) -> Result<(usize, usize), JsoncError> {
    if path.is_empty() {
        let bytes = blank_trailing_commas(&blank_comments(source)).into_bytes();
        let start = skip_space(&bytes, 0);
        if bytes.get(start) != Some(&b'{') {
            return Err(JsoncError::new("JSONC root must be an object"));
        }
        return Ok((start, matching_end(&bytes, start)?));
    }
    let (start, end) = value_span(source, path)?;
    let bytes = blank_trailing_commas(&blank_comments(source)).into_bytes();
    if bytes.get(start) != Some(&b'{') {
        return Err(JsoncError::new(format!(
            "{} must be an object",
            path.join(".")
        )));
    }
    Ok((start, end))
}

fn value_span(source: &str, path: &[String]) -> Result<(usize, usize), JsoncError> {
    let blanked = blank_trailing_commas(&blank_comments(source));
    let bytes = blanked.as_bytes();
    let mut object_start = skip_space(bytes, 0);
    if bytes.get(object_start) != Some(&b'{') {
        return Err(JsoncError::new("JSONC root must be an object"));
    }
    if path.is_empty() {
        return Ok((object_start, matching_end(bytes, object_start)?));
    }
    for (index, key) in path.iter().enumerate() {
        let members = members_from_blanked(bytes, object_start)?;
        let member = members
            .into_iter()
            .find(|member| member.key == *key)
            .ok_or_else(|| JsoncError::new(format!("missing JSONC path {}", path.join("."))))?;
        if index + 1 == path.len() {
            return Ok((member.value_start, member.value_end));
        }
        object_start = member.value_start;
        if bytes.get(object_start) != Some(&b'{') {
            return Err(JsoncError::new(format!("{key} must be an object")));
        }
    }
    Err(JsoncError::new("empty JSONC path"))
}

fn object_members(source: &str, object_start: usize) -> Result<Vec<Member>, JsoncError> {
    let blanked = blank_trailing_commas(&blank_comments(source));
    members_from_blanked(blanked.as_bytes(), object_start)
}

fn members_from_blanked(bytes: &[u8], object_start: usize) -> Result<Vec<Member>, JsoncError> {
    if bytes.get(object_start) != Some(&b'{') {
        return Err(JsoncError::new("member scan did not start at an object"));
    }
    let mut cursor = skip_space(bytes, object_start + 1);
    let mut members = Vec::new();
    while bytes.get(cursor) != Some(&b'}') {
        if bytes.get(cursor) != Some(&b'"') {
            return Err(JsoncError::new("expected an object member name"));
        }
        let key_start = cursor;
        let key_end = string_end(bytes, cursor)?;
        let key: String = serde_json::from_slice(&bytes[key_start..key_end])
            .map_err(|error| JsoncError::new(format!("invalid object key: {error}")))?;
        cursor = skip_space(bytes, key_end);
        if bytes.get(cursor) != Some(&b':') {
            return Err(JsoncError::new("expected ':' after object member name"));
        }
        let value_start = skip_space(bytes, cursor + 1);
        let value_end = json_value_end(bytes, value_start)?;
        cursor = skip_space(bytes, value_end);
        let comma = if bytes.get(cursor) == Some(&b',') {
            let comma = cursor;
            cursor = skip_space(bytes, cursor + 1);
            Some(comma)
        } else {
            None
        };
        members.push(Member {
            key,
            key_start,
            value_start,
            value_end,
            comma,
        });
        if comma.is_none() && bytes.get(cursor) != Some(&b'}') {
            return Err(JsoncError::new("expected ',' or '}' after object member"));
        }
    }
    Ok(members)
}

fn json_value_end(bytes: &[u8], start: usize) -> Result<usize, JsoncError> {
    let mut stream = serde_json::Deserializer::from_slice(&bytes[start..]).into_iter::<Value>();
    match stream.next() {
        Some(Ok(_)) => Ok(start + stream.byte_offset()),
        Some(Err(error)) => Err(JsoncError::new(format!("invalid JSON value: {error}"))),
        None => Err(JsoncError::new("missing JSON value")),
    }
}

fn matching_end(bytes: &[u8], start: usize) -> Result<usize, JsoncError> {
    json_value_end(bytes, start)
}

fn string_end(bytes: &[u8], start: usize) -> Result<usize, JsoncError> {
    let mut cursor = start + 1;
    let mut escaped = false;
    while let Some(byte) = bytes.get(cursor) {
        if escaped {
            escaped = false;
        } else if *byte == b'\\' {
            escaped = true;
        } else if *byte == b'"' {
            return Ok(cursor + 1);
        }
        cursor += 1;
    }
    Err(JsoncError::new("unterminated JSON string"))
}

fn insertion(
    source: &str,
    span: (usize, usize),
    members: &[Member],
    key: &str,
    value: &Value,
) -> Result<Edit, JsoncError> {
    let close = span
        .1
        .checked_sub(1)
        .ok_or_else(|| JsoncError::new("invalid object span"))?;
    let close_indent = line_indent(source, close);
    let child_indent = members
        .first()
        .map(|member| line_indent(source, member.key_start))
        .filter(|indent| indent.len() > close_indent.len())
        .unwrap_or_else(|| format!("{close_indent}  "));
    let rendered_key = serde_json::to_string(key)
        .map_err(|error| JsoncError::new(format!("render object key: {error}")))?;
    let rendered_value = render_value(value, child_indent.len())?;
    let prefix = if members.is_empty()
        || source[members.last().map_or(span.0 + 1, |member| member.value_end)..close].contains(',')
    {
        ""
    } else {
        ","
    };
    let interior = &source[span.0 + 1..close];
    let insertion = if interior.trim().is_empty() {
        format!("\n{child_indent}{rendered_key}: {rendered_value}\n{close_indent}")
    } else {
        let separator = if interior.ends_with('\n') { "" } else { "\n" };
        format!("{prefix}{separator}{child_indent}{rendered_key}: {rendered_value}\n{close_indent}")
    };
    Ok(Edit {
        start: close,
        end: close,
        replacement: insertion,
    })
}

fn removal(source: &str, members: &[Member], target: &Member) -> Edit {
    if let Some(comma) = target.comma {
        let mut end = comma + 1;
        if source.as_bytes().get(end) == Some(&b'\r') {
            end += 1;
        }
        if source.as_bytes().get(end) == Some(&b'\n') {
            end += 1;
        }
        Edit {
            start: target.key_start,
            end,
            replacement: String::new(),
        }
    } else if let Some(previous) = members
        .iter()
        .take_while(|member| member.key != target.key)
        .last()
    {
        let start = previous.comma.unwrap_or(target.key_start);
        Edit {
            start,
            end: target.value_end,
            replacement: String::new(),
        }
    } else {
        Edit {
            start: target.key_start,
            end: target.value_end,
            replacement: String::new(),
        }
    }
}

fn render_value(value: &Value, base_indent: usize) -> Result<String, JsoncError> {
    let rendered = serde_json::to_string_pretty(value)
        .map_err(|error| JsoncError::new(format!("render JSON value: {error}")))?;
    if !rendered.contains('\n') || base_indent == 0 {
        return Ok(rendered);
    }
    let padding = " ".repeat(base_indent);
    Ok(rendered.replace('\n', &format!("\n{padding}")))
}

fn indentation(source: &str, offset: usize) -> usize {
    line_indent(source, offset).len()
}

fn line_indent(source: &str, offset: usize) -> String {
    let line = source[..offset].rfind('\n').map_or(0, |index| index + 1);
    source[line..offset]
        .chars()
        .take_while(|character| matches!(character, ' ' | '\t'))
        .collect()
}

fn skip_space(bytes: &[u8], mut offset: usize) -> usize {
    while matches!(bytes.get(offset), Some(b' ' | b'\t' | b'\n' | b'\r')) {
        offset += 1;
    }
    offset
}

#[allow(dead_code)]
fn _assert_object(value: &Value) -> Option<&Map<String, Value>> {
    value.as_object()
}
