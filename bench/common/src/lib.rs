//! The fixed snippet shared by the modules and the oracle, so they agree byte for byte.

use std::sync::OnceLock;

/// Fixed RUM configuration: the modules have no configuration beyond on/off.
pub const SNIPPET_JSON: &[u8] = br#"{"majorVersion":6,"rum":{"applicationId":"00000000-0000-0000-0000-000000000000","clientToken":"pub00000000000000000000000000000000","site":"datadoghq.com","sessionSampleRate":100}}"#;

/// The snippet bytes, generated once per process and kept for its lifetime.
pub fn snippet() -> &'static [u8] {
    static SNIPPET: OnceLock<Vec<u8>> = OnceLock::new();
    SNIPPET.get_or_init(|| {
        let configuration = inject_browser_sdk::parse_configuration(SNIPPET_JSON)
            .expect("fixed snippet configuration parses");
        inject_browser_sdk::generate_snippet(&configuration).expect("fixed snippet generates")
    })
}
