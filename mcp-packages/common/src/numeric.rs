use crate::failure::ToolFailure;

/// Parse a JSON number parameter without silently falling back on invalid input.
///
/// Missing (`null`) values remain optional for callers to resolve with their documented default.
/// Non-negative integers, including zero, are accepted. Fractions, negative values, and other
/// JSON types return a typed tool failure.
pub fn parse_optional_u64(
    value: &serde_json::Value,
    name: &str,
) -> Result<Option<u64>, Box<dyn std::error::Error + Send + Sync>> {
    if value.is_null() {
        return Ok(None);
    }
    let n = value.as_f64().ok_or_else(|| {
        ToolFailure::new(
            "Numeric parameters must be non-negative integers.",
            format!("Error: '{name}' must be a non-negative integer, got {value}"),
        )
    })?;
    if n.fract() != 0.0 || n < 0.0 {
        return Err(ToolFailure::new(
            "Numeric parameters must be non-negative integers.",
            format!("Error: '{name}' must be a non-negative integer, got {n}"),
        )
        .into());
    }
    Ok(Some(n as u64))
}
