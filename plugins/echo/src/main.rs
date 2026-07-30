use spark_core::{Action, Candidate, Source};
use spark_ipc::{InvokeParams, InvokeResult, QueryParams, QueryResult};
use spark_sdk::{single_item, Plugin};

struct Echo;

impl Plugin for Echo {
    fn id(&self) -> &str {
        "com.spark.echo"
    }

    fn query(&mut self, params: QueryParams) -> QueryResult {
        let text = params.text;
        single_item(Candidate {
            id: format!("echo:{text}"),
            title: text.clone(),
            subtitle: Some("Echo · Enter 复制".into()),
            target: None,
            icon: None,
            score: 1.0,
            source: Source::Plugin,
            actions: vec![Action {
                id: "copy".into(),
                title: "复制".into(),
                is_default: true,
            }],
            plugin_id: Some(self.id().into()),
        })
    }

    fn invoke(&mut self, params: InvokeParams) -> InvokeResult {
        InvokeResult::CopyText { text: params.text }
    }
}

fn main() {
    // MVP: print one demo query result (full pipe loop comes with host IPC).
    let mut plugin = Echo;
    let result = plugin.query(QueryParams {
        text: std::env::args().nth(1).unwrap_or_else(|| "hello".into()),
        limit: 10,
    });
    println!("{}", serde_json::to_string_pretty(&result).unwrap());
}
