/// Chrome writes a JavaScript string's unpaired UTF-16 surrogate as an escape such as `\ud800`, which no Rust
/// string can hold, so the JSON decoder refuses the whole message. Returns the message with each unpaired
/// surrogate escape replaced by `�`, or `None` if it has none.
pub(crate) fn replace_unpaired(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] != b'\\' {
            i += 1;
            continue;
        }
        let Some(unit) = escaped_unit(bytes, i) else {
            i += 2;
            continue;
        };
        let paired = (0xD800..=0xDBFF).contains(&unit) && escaped_unit(bytes, i + 6).is_some_and(|low| (0xDC00..=0xDFFF).contains(&low));
        if paired {
            i += 12;
        } else if (0xD800..=0xDFFF).contains(&unit) {
            let out = out.get_or_insert_with(|| String::with_capacity(text.len()));
            out.push_str(&text[copied..i]);
            out.push_str("\\ufffd");
            i += 6;
            copied = i;
        } else {
            i += 6;
        }
    }
    out.map(|mut out| {
        out.push_str(&text[copied..]);
        out
    })
}

fn escaped_unit(bytes: &[u8], at: usize) -> Option<u16> {
    let escape = bytes.get(at..at + 6)?;
    if escape[0] != b'\\' || escape[1] != b'u' {
        return None;
    }
    u16::from_str_radix(std::str::from_utf8(&escape[2..]).ok()?, 16).ok()
}
