//! bindgen over the httpd/APR headers. Include paths come from `apxs` (apache2-dev), or from
//! `HTTPD_INCLUDES`/`APR_INCLUDES` when set.
use std::{env, path::PathBuf, process::Command};

fn apxs(query: &str) -> Option<String> {
    let out = Command::new("apxs").args(["-q", query]).output().ok()?;
    out.status.success().then(|| String::from_utf8_lossy(&out.stdout).trim().to_owned())
}

fn main() {
    println!("cargo:rerun-if-changed=wrapper.h");
    let httpd = env::var("HTTPD_INCLUDES").ok().or_else(|| apxs("INCLUDEDIR")).unwrap_or("/usr/include/apache2".into());
    let apr = env::var("APR_INCLUDES").ok().or_else(|| apxs("APR_INCLUDEDIR")).unwrap_or("/usr/include/apr-1.0".into());
    let mut builder = bindgen::Builder::default()
        .header("wrapper.h")
        .clang_arg(format!("-I{httpd}"))
        .clang_arg(format!("-I{apr}"))
        .allowlist_function("ap_(register_output_filter|remove_output_filter|pass_brigade|set_content_length|hook_insert_filter)")
        .allowlist_function("apr_(table_get|table_set|table_unset|bucket_heap_create|pool_cleanup_register|pool_cleanup_null)")
        .allowlist_function("apr_(brigade_create|bucket_immortal_create|bucket_pool_create|os_file_get|palloc)")
        .allowlist_type("module|request_rec|ap_filter_t|apr_bucket.*|ap_filter_type")
        .allowlist_var("apr_bucket_type_eos|apr_bucket_type_file|apr_bucket_type_flush|MODULE_MAGIC_.*|APR_HOOK_MIDDLE|APR_SUCCESS")
        .default_enum_style(bindgen::EnumVariation::Consts)
        .layout_tests(false);
    if let Some(cflags) = apxs("EXTRA_CPPFLAGS") {
        for flag in cflags.split_whitespace() {
            builder = builder.clang_arg(flag);
        }
    }
    let bindings = builder.generate().expect("bindgen over httpd headers");
    let out = PathBuf::from(env::var("OUT_DIR").unwrap()).join("httpd.rs");
    bindings.write_to_file(out).expect("write bindings");
}
