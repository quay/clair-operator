use webhook;

pub async fn app() -> axum::Router {
    let client = match kube::Client::try_default().await {
        Ok(c) => c,
        Err(e) => panic!("error starting webhook server: {e}"),
    };
    let s = webhook::State::new(client);
    webhook::app(s)
}
