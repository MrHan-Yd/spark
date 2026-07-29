use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Query {
    pub text: String,
    pub limit: u32,
}

impl Query {
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            limit: 50,
        }
    }

    pub fn normalized(&self) -> String {
        self.text.trim().to_lowercase()
    }
}
