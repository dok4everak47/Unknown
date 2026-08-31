use crate::message::Message;
use crate::model::{Model, Response};
use crate::runtime::{LocalRuntime, Runtime};
use crate::tool::Tool;

use std::fmt;
use std::path::PathBuf;

/// 单次用户输入最多允许的模型↔工具交互轮数，防止模型陷入循环。
const MAX_TOOL_ROUNDS: usize = 8;

/// Agent Loop 的致命错误。
///
/// 工具执行错误**不**属于此类：工具失败会作为 Tool Result 回传
/// 给 Model，由 Model 决定如何继续；只有 Model 请求失败或超过
/// 最大工具轮数才会让整轮输入终止。
#[derive(Debug)]
pub enum AgentError {
    /// 模型连续返回工具调用超过 [`MAX_TOOL_ROUNDS`] 轮。
    TooManyToolRounds,
    /// 模型请求失败（网络 / API / 解析错误）。
    Model(Box<dyn std::error::Error>),
}

impl fmt::Display for AgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AgentError::TooManyToolRounds => {
                write!(f, "too many tool-call rounds, giving up")
            }
            AgentError::Model(err) => write!(f, "{err}"),
        }
    }
}

impl std::error::Error for AgentError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            AgentError::Model(err) => Some(err.as_ref()),
            AgentError::TooManyToolRounds => None,
        }
    }
}

/// Agent：持有 Model 与工具执行根目录，协调一次用户输入的多轮交互。
///
/// 结构上只依赖 [`Model`] trait 与 [`Tool`] 的静态接口，因此可以用
/// fake Model 做单元测试，不需要真实 API key。
pub struct Agent<M: Model> {
    model: M,
    /// 所有文件工具的执行边界（与当前 `Tool::execute` 的 cwd 语义一致）。
    root: PathBuf,
    /// 工具执行的副作用 Runtime（默认 [`LocalRuntime`]，测试可注入 fake）。
    runtime: Box<dyn Runtime>,
}

impl<M: Model> Agent<M> {
    /// 以当前工作目录作为工具执行根目录创建 Agent。
    pub fn new(model: M) -> Result<Self, std::io::Error> {
        let root = std::env::current_dir()?;
        Ok(Self {
            model,
            root,
            runtime: Box::new(LocalRuntime),
        })
    }

    /// 测试辅助（仅测试构建）：在指定根目录下创建 Agent。
    ///
    /// 供 model.rs 的集成测试用 mock server 驱动真实 `OpenAICompatibleModel`。
    #[cfg(test)]
    pub(crate) fn new_with_root_for_test(model: M, root: PathBuf) -> Self {
        Self {
            model,
            root,
            runtime: Box::new(LocalRuntime),
        }
    }

    /// 测试辅助（仅测试构建）：注入自定义 Runtime（如 [`FakeRuntime`]）。
    ///
    /// 供 agent.rs 测试验证工具执行确实经由注入的 Runtime 完成。
    #[cfg(test)]
    pub(crate) fn new_with_runtime_for_test(
        model: M,
        root: PathBuf,
        runtime: Box<dyn Runtime>,
    ) -> Self {
        Self {
            model,
            root,
            runtime,
        }
    }

    /// 处理一次用户输入：可能触发多轮 model ↔ tool 交互，
    /// 直到模型给出文本回答。
    ///
    /// - 成功：模型已给出文本回答（已 push 进 `conversation`），返回 `Ok(())`。
    /// - 失败：`TooManyToolRounds` 或 `Model` 错误；此时 `conversation`
    ///   已回滚到本输入开始之前，保持对话状态一致。
    ///
    /// 工具执行错误不会中止本方法：它们作为 Tool Result 回传给 Model。
    pub fn run_turn(
        &self,
        conversation: &mut Vec<Message>,
        user_text: &str,
    ) -> Result<(), AgentError> {
        // 记录本回合开始的位置，出错时回滚到此处
        let turn_start = conversation.len();
        conversation.push(Message::user(user_text));

        let mut tool_rounds = 0;

        loop {
            match self.model.complete(conversation) {
                Ok(Response::Text(answer)) => {
                    conversation.push(Message::assistant(answer));
                    return Ok(());
                }
                Ok(Response::ToolCall(calls)) => {
                    tool_rounds += 1;
                    if tool_rounds > MAX_TOOL_ROUNDS {
                        conversation.truncate(turn_start);
                        return Err(AgentError::TooManyToolRounds);
                    }

                    conversation.push(Message::assistant_tool_calls(calls.clone()));
                    for call in calls {
                        let result = match Tool::from_call(&call.name, &call.arguments) {
                            Ok(tool) => match tool.execute(self.runtime.as_ref(), &self.root) {
                                Ok(text) => text,
                                Err(err) => format!("tool error: {err}"),
                            },
                            Err(err) => format!("tool error: {err}"),
                        };
                        conversation.push(Message::tool(result, call.id));
                    }
                }
                Err(err) => {
                    // 丢弃本回合产生的消息，保持对话状态一致
                    conversation.truncate(turn_start);
                    return Err(AgentError::Model(err));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Role, ToolCall};
    use crate::model::Response;
    use crate::runtime::{ExecError, ExecOutput, RuntimeEntry};
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::fs;
    use std::io;
    use std::path::Path;
    use std::rc::Rc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 每个测试一个独立的临时目录，避免并行测试互相干扰。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("myagent-agent-test-{}-{n}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 在指定根目录下创建 Agent（测试辅助，便于隔离工具副作用）。
    fn new_with_root<M: Model>(model: M, root: PathBuf) -> Agent<M> {
        Agent {
            model,
            root,
            runtime: Box::new(LocalRuntime),
        }
    }

    /// 可编程 fake Model：按顺序返回预设的 [`Response`]，并记录每次收到的 messages。
    struct FakeModel {
        responses: RefCell<std::collections::VecDeque<Response>>,
        seen: RefCell<Vec<Vec<Message>>>,
    }

    impl FakeModel {
        fn new(responses: Vec<Response>) -> Self {
            Self {
                responses: RefCell::new(responses.into()),
                seen: RefCell::new(Vec::new()),
            }
        }

        fn calls(&self) -> usize {
            self.seen.borrow().len()
        }

        fn last_messages(&self) -> Vec<Message> {
            self.seen
                .borrow()
                .last()
                .cloned()
                .unwrap_or_default()
        }
    }

    impl Model for FakeModel {
        fn complete(&self, messages: &[Message]) -> Result<Response, Box<dyn std::error::Error>> {
            self.seen
                .borrow_mut()
                .push(messages.to_vec());
            Ok(self
                .responses
                .borrow_mut()
                .pop_front()
                .expect("fake model exhausted"))
        }
    }

    /// 总是返回错误的 Model，用于测试错误传播。
    struct FailingModel;

    impl Model for FailingModel {
        fn complete(&self, _messages: &[Message]) -> Result<Response, Box<dyn std::error::Error>> {
            Err("model exploded".into())
        }
    }

    /// 构造一个 write_file 的 ToolCall。
    fn tool_call(id: &str, name: &str, args: serde_json::Value) -> ToolCall {
        ToolCall {
            id: id.to_string(),
            name: name.to_string(),
            arguments: args,
        }
    }

    #[test]
    fn text_response_is_returned_and_appended() {
        let root = temp_root();
        let model = FakeModel::new(vec![Response::Text("hello back".to_string())]);
        let agent = new_with_root(model, root);

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "hi")
            .unwrap();

        // user 消息 + assistant 回答
        assert_eq!(conversation.len(), 2);
        assert_eq!(conversation[0].role, Role::User);
        assert_eq!(conversation[0].content, "hi");
        assert_eq!(conversation[1].role, Role::Assistant);
        assert_eq!(conversation[1].content, "hello back");
        // 模型只被调用一次
        assert_eq!(agent.model.calls(), 1);
    }

    #[test]
    fn single_tool_call_then_final_text() {
        let root = temp_root();
        let model = FakeModel::new(vec![
            Response::ToolCall(vec![tool_call(
                "call_1",
                "write_file",
                serde_json::json!({ "path": "out.txt", "content": "data" }),
            )]),
            Response::Text("done".to_string()),
        ]);
        let agent = new_with_root(model, root.clone());

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "write a file")
            .unwrap();

        // 文件确实被写入
        let written = fs::read_to_string(root.join("out.txt")).unwrap();
        assert_eq!(written, "data");

        // conversation: user, assistant(tool_calls), tool(result), assistant(final)
        assert_eq!(conversation.len(), 4);
        assert_eq!(conversation[1].role, Role::Assistant);
        assert_eq!(conversation[1].tool_calls.len(), 1);
        assert_eq!(conversation[2].role, Role::Tool);
        assert!(conversation[2].content.contains("out.txt"));
        assert_eq!(conversation[2].tool_call_id.as_deref(), Some("call_1"));
        assert_eq!(conversation[3].content, "done");

        // 第二次模型调用应看到工具结果（user + assistant tool_calls + tool result）
        let second = agent.model.last_messages();
        assert_eq!(second.len(), 3);
        assert!(second.iter().any(|m| m.role == Role::Tool));
    }

    #[test]
    fn multiple_tool_calls_rounds() {
        let root = temp_root();
        let model = FakeModel::new(vec![
            Response::ToolCall(vec![tool_call(
                "call_1",
                "write_file",
                serde_json::json!({ "path": "a.txt", "content": "1" }),
            )]),
            Response::ToolCall(vec![tool_call(
                "call_2",
                "write_file",
                serde_json::json!({ "path": "b.txt", "content": "2" }),
            )]),
            Response::Text("both done".to_string()),
        ]);
        let agent = new_with_root(model, root.clone());

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "write two files")
            .unwrap();

        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "2");
        assert_eq!(agent.model.calls(), 3);
        // user + 2× assistant(tool_calls) + 2× tool + assistant(final)
        assert_eq!(conversation.len(), 6);
    }

    #[test]
    fn multiple_tools_in_one_call_batch() {
        let root = temp_root();
        let model = FakeModel::new(vec![
            Response::ToolCall(vec![
                tool_call(
                    "call_1",
                    "write_file",
                    serde_json::json!({ "path": "a.txt", "content": "1" }),
                ),
                tool_call(
                    "call_2",
                    "write_file",
                    serde_json::json!({ "path": "b.txt", "content": "2" }),
                ),
            ]),
            Response::Text("done".to_string()),
        ]);
        let agent = new_with_root(model, root.clone());

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "write two files at once")
            .unwrap();

        assert_eq!(fs::read_to_string(root.join("a.txt")).unwrap(), "1");
        assert_eq!(fs::read_to_string(root.join("b.txt")).unwrap(), "2");
    }

    #[test]
    fn tool_error_is_returned_to_model_not_crash() {
        let root = temp_root();
        // read_file 不存在的文件 → ToolError::NotFound → 作为 tool error 回传
        let model = FakeModel::new(vec![
            Response::ToolCall(vec![tool_call(
                "call_1",
                "read_file",
                serde_json::json!({ "path": "missing.txt" }),
            )]),
            Response::Text("file not found, I see".to_string()),
        ]);
        let agent = new_with_root(model, root);

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "read missing")
            .unwrap();

        // 工具结果包含错误信息，且循环继续直到模型回答
        let tool_msg = &conversation[2];
        assert_eq!(tool_msg.role, Role::Tool);
        assert!(
            tool_msg.content.contains("tool error"),
            "got: {}",
            tool_msg.content
        );
        assert_eq!(conversation[3].content, "file not found, I see");
    }

    #[test]
    fn model_error_is_propagated_and_conversation_rolled_back() {
        let root = temp_root();
        let agent = new_with_root(FailingModel, root);

        let mut conversation = vec![Message::assistant("old context")];
        let result = agent.run_turn(&mut conversation, "hi");

        assert!(matches!(result, Err(AgentError::Model(_))));
        // 回滚：本回合的 user 消息被移除，原上下文保留
        assert_eq!(conversation.len(), 1);
        assert_eq!(conversation[0].content, "old context");
    }

    #[test]
    fn too_many_tool_rounds_is_capped() {
        let root = temp_root();
        // 永远返回 ToolCall 的模型
        let mut responses = Vec::new();
        for i in 0..=MAX_TOOL_ROUNDS {
            responses.push(Response::ToolCall(vec![tool_call(
                &format!("call_{i}"),
                "write_file",
                serde_json::json!({ "path": "loop.txt", "content": "x" }),
            )]));
        }
        let model = FakeModel::new(responses);
        let agent = new_with_root(model, root);

        let mut conversation = Vec::new();
        let result = agent
            .run_turn(&mut conversation, "loop forever")
            .unwrap_err();

        assert!(matches!(result, AgentError::TooManyToolRounds));
        // 回滚到输入之前：conversation 应只剩原有内容（空）
        assert!(conversation.is_empty());
    }

    #[test]
    fn dispatches_different_tool_types() {
        let root = temp_root();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), "fn main() {}").unwrap();

        let model = FakeModel::new(vec![
            // 一轮内连续调度 read_file 与 search 两种工具
            Response::ToolCall(vec![
                tool_call(
                    "call_1",
                    "read_file",
                    serde_json::json!({ "path": "src/a.rs" }),
                ),
                tool_call("call_2", "search", serde_json::json!({ "query": "main" })),
            ]),
            Response::Text("both worked".to_string()),
        ]);
        let agent = new_with_root(model, root);

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "inspect")
            .unwrap();

        assert_eq!(conversation[2].role, Role::Tool);
        assert_eq!(conversation[2].content, "fn main() {}");
        assert!(
            conversation[3]
                .content
                .contains("src/a.rs:1"),
            "got: {}",
            conversation[3].content
        );
        assert_eq!(conversation[4].content, "both worked");
    }

    /// 可编程 fake Runtime：脚本化返回文件内容 / 命令结果，不触碰真实文件系统。
    ///
    /// 每次调用都记录到共享调用日志（`Rc<RefCell<Vec<String>>>`），供测试
    /// 断言工具执行确实经由注入的 Runtime 完成。
    struct FakeRuntime {
        /// path（文件名）→ 脚本化文件内容（`read_file` 命中时返回）。
        files: HashMap<String, String>,
        /// 调用日志：`"read_file:<resolved-path>"` 形式的记录。
        log: Rc<RefCell<Vec<String>>>,
    }

    impl FakeRuntime {
        fn new() -> Self {
            Self {
                files: HashMap::new(),
                log: Rc::new(RefCell::new(Vec::new())),
            }
        }

        /// 脚本化一个文件的内容（按文件名匹配）。
        fn with_file(mut self, name: &str, content: &str) -> Self {
            self.files
                .insert(name.to_string(), content.to_string());
            self
        }

        /// 共享调用日志（跨 Box 边界读取）。
        fn log(&self) -> Rc<RefCell<Vec<String>>> {
            Rc::clone(&self.log)
        }
    }

    impl Runtime for FakeRuntime {
        fn read_file(&self, path: &Path) -> io::Result<String> {
            self.log
                .borrow_mut()
                .push(format!("read_file:{}", path.display()));
            let key = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or_default();
            self.files
                .get(key)
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "fake runtime: not found"))
        }

        fn write_file(&self, path: &Path, _content: &str) -> io::Result<()> {
            self.log
                .borrow_mut()
                .push(format!("write_file:{}", path.display()));
            Ok(())
        }

        fn read_dir(&self, path: &Path) -> io::Result<Vec<RuntimeEntry>> {
            self.log
                .borrow_mut()
                .push(format!("read_dir:{}", path.display()));
            Ok(Vec::new())
        }

        fn exec(
            &self,
            program: &str,
            _args: &[String],
            cwd: &Path,
        ) -> Result<ExecOutput, ExecError> {
            self.log
                .borrow_mut()
                .push(format!("exec:{program} cwd={}", cwd.display()));
            Ok(ExecOutput {
                code: 0,
                output: "fake exec ok".to_string(),
            })
        }
    }

    #[test]
    fn tool_execution_goes_through_injected_runtime() {
        let root = temp_root();
        // 真实文件系统上的 note.txt 内容与 FakeRuntime 脚本化的内容不同：
        // 若工具绕过 Runtime 直接读文件，会得到 "real content"。
        fs::write(root.join("note.txt"), "real content").unwrap();
        let model = FakeModel::new(vec![
            Response::ToolCall(vec![tool_call(
                "call_1",
                "read_file",
                serde_json::json!({ "path": "note.txt" }),
            )]),
            Response::Text("got it".to_string()),
        ]);
        let runtime = FakeRuntime::new().with_file("note.txt", "scripted content");
        let log = runtime.log();
        let agent = Agent::new_with_runtime_for_test(model, root, Box::new(runtime));

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "read the note")
            .unwrap();

        // 工具结果来自 FakeRuntime 的脚本化内容，而不是真实文件系统（"real content"）
        assert_eq!(conversation[2].content, "scripted content");
        // 调用日志证明 read_file 确实经由注入的 Runtime 完成
        let calls = log.borrow();
        assert_eq!(calls.len(), 1, "unexpected calls: {calls:?}");
        assert!(
            calls[0].starts_with("read_file:") && calls[0].ends_with("note.txt"),
            "got: {}",
            calls[0]
        );
    }
}
