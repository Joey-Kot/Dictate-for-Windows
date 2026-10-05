use std::time::Duration;

pub(crate) fn client(config: &crate::Config) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder = reqwest::Client::builder()
        .danger_accept_invalid_certs(!config.verify_ssl)
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .no_gzip()
        .no_brotli()
        .no_deflate();
    if config.request_timeout > 0 {
        builder = builder.timeout(Duration::from_secs(config.request_timeout as u64));
    }
    if !config.enable_http2 {
        builder = builder.http1_only();
    }
    builder.build()
}
