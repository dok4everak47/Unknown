pub struct Prompt {
    pub text: String,
}

impl Prompt {
    pub fn new(text: String) -> Self {
        Self { text }
    }
}
