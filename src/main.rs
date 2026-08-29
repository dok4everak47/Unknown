mod model;
mod prompt;

use crate::model::{Model, OpenAICompatibleModel};
use crate::prompt::Prompt;
use std::env;

fn main() {
    let text = env::args()
        .skip(1)
        .collect::<Vec<_>>()
        .join(" ");

    if text.is_empty() {
        eprintln!("Usage: myagent <prompt>");
        std::process::exit(2);
    }

    let prompt = Prompt::new(text);

    let api_key = match env::var("OPENAI_API_KEY") {
        Ok(key) => key,
        Err(_) => {
            eprintln!("OPENAI_API_KEY is not set");
            std::process::exit(1);
        }
    };

    let model = OpenAICompatibleModel::new(api_key);

    match model.complete(&prompt) {
        Ok(response) => println!("{}", response.text),
        Err(err) => {
            eprintln!("model request failed: {err}");
            std::process::exit(1);
        }
    }
}
