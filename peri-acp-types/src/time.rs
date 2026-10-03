//! Calendar time displayed in prompts and host metadata.

/// Format the current time using the deployment's calendar convention.
/// Native deployments retain the machine's local zone. Emscripten deployments
/// use UTC so their result does not depend on a JavaScript timezone probe.
pub fn format_now(format: &str) -> String {
    #[cfg(target_os = "emscripten")]
    {
        chrono::Utc::now().format(format).to_string()
    }
    #[cfg(not(target_os = "emscripten"))]
    {
        chrono::Local::now().format(format).to_string()
    }
}
