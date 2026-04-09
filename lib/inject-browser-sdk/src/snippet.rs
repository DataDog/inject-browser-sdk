// Unless explicitly stated otherwise all files in this repository are licensed
// under the Apache 2.0 License. This product includes software developed at
// Datadog (https://www.datadoghq.com/).
//
// Copyright 2024-Present Datadog, Inc.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::{configuration::parse_site, configuration::Configuration, error::Error};

/// Generates the Datadog RUM SDK JavaScript snippet based on the provided configuration.
///
/// This function creates a JavaScript snippet that can be injected into your web application
/// to enable RUM. The snippet is generated using the provided `configuration`.
pub fn generate_snippet(configuration: &Configuration) -> Result<Vec<u8>, Error> {
    configuration.validate()?;

    let site = configuration.rum.site.as_deref().unwrap_or("datadoghq.com");
    let url = format_cdn_url(configuration.major_version, site)?;

    let mut output = Vec::new();

    output.extend_from_slice(
        br#"
<script>
(function(h,o,u,n,d) {
  h=h[d]=h[d]||{q:[],onReady:function(c){h.q.push(c)}}
  d=o.createElement(u);d.async=1;d.src=n
  n=o.getElementsByTagName(u)[0];n.parentNode.insertBefore(d,n)
})(window,document,'script','"#,
    );

    output.extend_from_slice(url.as_bytes());

    output.extend_from_slice(
        br#"','DD_RUM')
window.DD_RUM.onReady(function() {
  window.DD_RUM.init("#,
    );

    let mut rum_value =
        serde_json::to_value(&configuration.rum).map_err(|e| Error::Json(e.to_string()))?;
    // Deterministic key ordering, independent of serde_json's `preserve_order` feature.
    rum_value.sort_all_objects();
    let json_string = serde_json::to_string(&rum_value).map_err(|e| Error::Json(e.to_string()))?;
    for ch in json_string.chars() {
        if ch.is_ascii() {
            let mut buf = [0; 4];
            output.extend_from_slice(ch.encode_utf8(&mut buf).as_bytes());
        } else {
            let mut buf = [0; 2];
            for escape in ch.encode_utf16(&mut buf) {
                output.extend_from_slice(format!("\\u{:04x}", escape).as_bytes());
            }
        }
    }

    output.extend_from_slice(
        br#");
});
</script>
"#,
    );

    Ok(output)
}

fn format_cdn_url(major_version: u32, site: &str) -> Result<String, Error> {
    let parsed = parse_site(site).ok_or_else(|| Error::UnsupportedSite(site.to_string()))?;

    // US1-FED uses a flat URL without region prefix (per browser-sdk README)
    #[cfg(feature = "support_gov_domain")]
    if parsed.domain.starts_with("ddog-gov.") {
        return Ok(format!(
            "https://www.datadoghq-browser-agent.com/datadog-rum-v{major_version}.js"
        ));
    }

    let region = match parsed.prefix {
        Some(prefix) => prefix,
        None if parsed.domain.ends_with(".eu") => "eu1",
        None => "us1",
    };

    Ok(format!(
        "https://www.datadoghq-browser-agent.com/{region}/v{major_version}/datadog-rum.js"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::configuration::RumConfiguration;
    use alloc::boxed::Box;
    use alloc::collections::BTreeMap;
    use pretty_assertions::assert_eq;
    use serde_json::json;

    #[test]
    fn test_generate_snippet_with_minimal_configuration() {
        let expected_snippet = r#"
<script>
(function(h,o,u,n,d) {
  h=h[d]=h[d]||{q:[],onReady:function(c){h.q.push(c)}}
  d=o.createElement(u);d.async=1;d.src=n
  n=o.getElementsByTagName(u)[0];n.parentNode.insertBefore(d,n)
})(window,document,'script','https://www.datadoghq-browser-agent.com/us1/v5/datadog-rum.js','DD_RUM')
window.DD_RUM.onReady(function() {
  window.DD_RUM.init({"applicationId":"bar","clientToken":"foo"});
});
</script>
"#;

        assert_eq!(
            core::str::from_utf8(
                &generate_snippet(&Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("bar")),
                        client_token: Some(String::from("foo")),
                        ..Default::default()
                    },
                })
                .unwrap()
            )
            .unwrap(),
            expected_snippet
        )
    }

    #[test]
    fn test_generate_snippet() {
        let expected_snippet = r#"
<script>
(function(h,o,u,n,d) {
  h=h[d]=h[d]||{q:[],onReady:function(c){h.q.push(c)}}
  d=o.createElement(u);d.async=1;d.src=n
  n=o.getElementsByTagName(u)[0];n.parentNode.insertBefore(d,n)
})(window,document,'script','https://www.datadoghq-browser-agent.com/us1/v5/datadog-rum.js','DD_RUM')
window.DD_RUM.onReady(function() {
  window.DD_RUM.init({"applicationId":"bar","clientToken":"foo","defaultPrivacyLevel":"mask","sessionSampleRate":42.42,"site":"datadoghq.com","trackResources":true});
});
</script>
"#;

        assert_eq!(
            core::str::from_utf8(
                &generate_snippet(&Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("bar")),
                        client_token: Some(String::from("foo")),
                        site: Some(String::from("datadoghq.com")),
                        default_privacy_level: Some(String::from("mask")),
                        track_resources: Some(true),
                        session_sample_rate: Some(42.42),
                        ..Default::default()
                    },
                })
                .unwrap()
            )
            .unwrap(),
            expected_snippet
        )
    }

    #[test]
    fn unicode_values() {
        // It is important that all non-ascii values are escaped, because we don't know the page
        // encoding.
        let expected_snippet = r#"
<script>
(function(h,o,u,n,d) {
  h=h[d]=h[d]||{q:[],onReady:function(c){h.q.push(c)}}
  d=o.createElement(u);d.async=1;d.src=n
  n=o.getElementsByTagName(u)[0];n.parentNode.insertBefore(d,n)
})(window,document,'script','https://www.datadoghq-browser-agent.com/us1/v5/datadog-rum.js','DD_RUM')
window.DD_RUM.onReady(function() {
  window.DD_RUM.init({"applicationId":"\u263a \u20ac \u00e9","clientToken":"foo","site":"datadoghq.com"});
});
</script>
"#;

        assert_eq!(
            core::str::from_utf8(
                &generate_snippet(&Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("☺ € é")),
                        client_token: Some(String::from("foo")),
                        site: Some(String::from("datadoghq.com")),
                        ..Default::default()
                    },
                })
                .unwrap()
            )
            .unwrap(),
            expected_snippet
        )
    }

    #[test]
    fn test_forward_compatibility() {
        let expected_snippet = r#"
<script>
(function(h,o,u,n,d) {
  h=h[d]=h[d]||{q:[],onReady:function(c){h.q.push(c)}}
  d=o.createElement(u);d.async=1;d.src=n
  n=o.getElementsByTagName(u)[0];n.parentNode.insertBefore(d,n)
})(window,document,'script','https://www.datadoghq-browser-agent.com/us1/v5/datadog-rum.js','DD_RUM')
window.DD_RUM.onReady(function() {
  window.DD_RUM.init({"applicationId":"foo","clientToken":"bar","newopt":"value","newopt2":true,"site":"datadoghq.com"});
});
</script>
"#;

        assert_eq!(
            core::str::from_utf8(
                &generate_snippet(&Configuration {
                    major_version: 5,
                    rum: RumConfiguration {
                        application_id: Some(String::from("foo")),
                        client_token: Some(String::from("bar")),
                        site: Some(String::from("datadoghq.com")),
                        other: BTreeMap::from([
                            (Box::from("newopt"), json!("value")),
                            (Box::from("newopt2"), json!(true)),
                        ]),
                        ..Default::default()
                    },
                })
                .unwrap()
            )
            .unwrap(),
            expected_snippet
        )
    }

    #[test]
    fn test_format_cdn_url() {
        assert_eq!(
            format_cdn_url(5, "datadoghq.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/us1/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "datadoghq.eu").unwrap(),
            "https://www.datadoghq-browser-agent.com/eu1/v5/datadog-rum.js"
        );
        // explicit us1 prefix produces the same URL as bare datadoghq.com
        assert_eq!(
            format_cdn_url(5, "us1.datadoghq.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/us1/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "us3.datadoghq.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/us3/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "us5.datadoghq.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/us5/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "ap1.datadoghq.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/ap1/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "ap2.datadoghq.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/ap2/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "eu2.datadoghq.eu").unwrap(),
            "https://www.datadoghq-browser-agent.com/eu2/v5/datadog-rum.js"
        );
        // datad0g (staging) domains use the same CDN as production
        assert_eq!(
            format_cdn_url(5, "datad0g.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/us1/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "datad0g.eu").unwrap(),
            "https://www.datadoghq-browser-agent.com/eu1/v5/datadog-rum.js"
        );
        assert_eq!(
            format_cdn_url(5, "us3.datad0g.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/us3/v5/datadog-rum.js"
        );
        // major version 6
        assert_eq!(
            format_cdn_url(6, "datadoghq.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/us1/v6/datadog-rum.js"
        );
        // rejected sites
        assert_eq!(
            format_cdn_url(5, "foo.com").unwrap_err(),
            Error::UnsupportedSite(String::from("foo.com"))
        );
    }

    #[test]
    fn test_generate_snippet_with_prefixed_site() {
        let snippet = generate_snippet(&Configuration {
            major_version: 5,
            rum: RumConfiguration {
                client_token: Some(String::from("tok")),
                application_id: Some(String::from("app")),
                site: Some(String::from("us5.datadoghq.com")),
                ..Default::default()
            },
        })
        .unwrap();
        let s = core::str::from_utf8(&snippet).unwrap();
        assert!(
            s.contains("datadoghq-browser-agent.com/us5/v5/datadog-rum.js"),
            "expected us5 region in CDN URL"
        );
        assert!(
            s.contains(r#""site":"us5.datadoghq.com""#),
            "expected site in init config"
        );
    }

    #[cfg(feature = "support_gov_domain")]
    #[test]
    fn test_format_cdn_url_gov() {
        // US1-FED uses a flat URL without region prefix (per browser-sdk README)
        assert_eq!(
            format_cdn_url(5, "ddog-gov.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/datadog-rum-v5.js"
        );
        assert_eq!(
            format_cdn_url(6, "ddog-gov.com").unwrap(),
            "https://www.datadoghq-browser-agent.com/datadog-rum-v6.js"
        );
        assert_eq!(
            format_cdn_url(5, "ddog-gov.mil").unwrap(),
            "https://www.datadoghq-browser-agent.com/datadog-rum-v5.js"
        );
    }
}
