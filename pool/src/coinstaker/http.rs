use anyhow::Result;

use url::Url;

// send webhook message to registered endpoints
pub struct Webhook {
    client: reqwest::Client,
    endpoints: Vec<Url>,
}

pub struct WebhookMessage {}

impl Webhook {
    pub fn new(endpoints: Vec<Url>) -> Result<Self> {
        let client = reqwest::ClientBuilder::new().build()?;

        Ok(Self { client, endpoints })
    }

    pub async fn send(&self, _msg: WebhookMessage) -> Result<()> {
        for endpoint in self.endpoints.clone() {
            self.client.post(endpoint).body("test").send().await?;
        }

        Ok(())
    }
}
