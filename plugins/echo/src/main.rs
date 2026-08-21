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
                target: None,
            }],
            plugin_id: Some(self.id().into()),
        })
    }

    fn invoke(&mut self, params: InvokeParams) -> InvokeResult {
        InvokeResult::CopyText { text: params.text }
    }
}

fn main() {
    // 阻塞运行：从 stdin 读 JSON-RPC 帧 → query/invoke → 向 stdout 写响应。
    // stdout 必须纯净协议帧，日志走 stderr。
    let mut plugin = Echo;
    if let Err(e) = spark_sdk::run_loop(&mut plugin) {
        eprintln!("spark-plugin-echo: run_loop exited: {e}");
        std::process::exit(1);
    }
}
