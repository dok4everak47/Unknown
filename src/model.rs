use crate::message::{Message, Role, ToolCall};
use crate::tool;
use serde::{Deserialize, Serialize};
use std::env;
use std::io::BufRead;
use std::time::Duration;

/// 模型回复：要么是一段文本，要么是一组工具调用请求。
#[derive(Debug, Clone, PartialEq)]
pub enum Response {
    Text(String),
    ToolCall(Vec<ToolCall>),
}

/// 流式模型事件：供 UI 增量显示。
///
/// - `TextDelta`：正式回答的文本增量（进入对话历史）。
/// - `ReasoningDelta`：推理过程增量——**纯展示用，不落对话历史**。
///   模型的 `reasoning_content` 只经事件流"路过"显示，由 UI 决定是否打印；
///   累加器（content / tool_calls）绝不吸收它，历史行为零变化。
#[derive(Debug, Clone, PartialEq)]
pub enum ModelEvent {
    TextDelta(String),
    ReasoningDelta(String),
}

/// 模型抽象：任何可对话的模型都实现此 trait
pub trait Model {
    /// 非流式完整回复。
    fn complete(&self, messages: &[Message]) -> Result<Response, Box<dyn std::error::Error>>;

    /// 流式回复：文本增量经 `on_event` 逐段发出。
    ///
    /// 默认实现走非流式 [`Model::complete`]，把最终文本作为单个 delta 发出，
    /// 因此 fake Model 无需额外实现即可获得流式接口（现有测试零改动）。
    fn complete_streaming(
        &self,
        messages: &[Message],
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> Result<Response, Box<dyn std::error::Error>> {
        let response = self.complete(messages)?;
        if let Response::Text(text) = &response {
            on_event(ModelEvent::TextDelta(text.clone()));
        }
        Ok(response)
    }
}

/// OpenAI 兼容 API 客户端（reqwest blocking）
pub struct OpenAICompatibleModel {
    pub api_key: String,
    pub base_url: String,
    pub model: String,
}

// ---------------- API 层：消息 ----------------

/// API 层专用的消息结构，独立于核心 `Message` 类型。
///
/// 核心 `Message`（Role 枚举、`ToolCall`）与 OpenAI 兼容 API 的字符串角色
/// 是两个不同的概念，由 `from_core` 负责转换。
#[derive(Debug, Serialize)]
struct ApiMessage {
    role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tool_calls: Vec<ApiToolCall>,
}

impl ApiMessage {
    fn from_core(message: &Message) -> Self {
        let role = match message.role {
            Role::User => "user",
            Role::Assistant => "assistant",
            Role::Tool => "tool",
        };

        // 工具调用消息没有文本内容，content 用 null 表示
        let content = if message.content.is_empty() && !message.tool_calls.is_empty() {
            None
        } else {
            Some(message.content.clone())
        };

        Self {
            role: role.to_string(),
            content,
            tool_call_id: message.tool_call_id.clone(),
            tool_calls: message
                .tool_calls
                .iter()
                .map(ApiToolCall::from_core)
                .collect(),
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiToolCall {
    id: String,
    #[serde(rename = "type")]
    kind: String,
    function: ApiFunctionCall,
}

impl ApiToolCall {
    fn from_core(call: &ToolCall) -> Self {
        Self {
            id: call.id.clone(),
            kind: "function".to_string(),
            function: ApiFunctionCall {
                name: call.name.clone(),
                // OpenAI 兼容 API 的 function.arguments 是 JSON 字符串，不是对象
                arguments: serde_json::to_string(&call.arguments)
                    .unwrap_or_else(|_| "{}".to_string()),
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct ApiFunctionCall {
    name: String,
    arguments: String,
}

// ---------------- API 层：请求 ----------------

#[derive(Debug, Serialize)]
struct ChatRequest {
    model: String,
    messages: Vec<ApiMessage>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tools: Vec<ApiTool>,
    /// SSE 流式开关；非流式请求（`false`）序列化时省略，与既有请求形状一致。
    #[serde(skip_serializing_if = "is_false")]
    stream: bool,
}

fn is_false(v: &bool) -> bool {
    !*v
}

#[derive(Debug, Serialize)]
struct ApiTool {
    #[serde(rename = "type")]
    kind: String,
    function: ApiFunction,
}

#[derive(Debug, Serialize)]
struct ApiFunction {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// ---------------- API 层：响应 ----------------

#[derive(Debug, Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
}

#[derive(Debug, Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Debug, Deserialize)]
struct ResponseMessage {
    content: Option<String>,
    tool_calls: Option<Vec<ResponseToolCall>>,
}

#[derive(Debug, Deserialize)]
struct ResponseToolCall {
    id: String,
    function: ResponseFunction,
}

#[derive(Debug, Deserialize)]
struct ResponseFunction {
    name: String,
    arguments: String,
}

// ---------------- SSE 流式：累加器与分片结构 ----------------

/// SSE 流式增量累加器：流结束后转成 [`Response`]。
///
/// `reasoning_content` **不进入**累加器：即使流中包含推理过程，也绝不追加到
/// `content` / 对话历史。
#[derive(Debug, Default)]
pub struct StreamAccumulator {
    /// 累加的文本内容。
    pub content: String,
    /// 按 `index` 分片累加的工具调用。
    pub tool_calls: Vec<StreamToolCall>,
}

/// 一个按 `index` 累加的工具调用。
#[derive(Debug, Default)]
pub struct StreamToolCall {
    /// 分片归属的工具调用序号（`choices[0].delta.tool_calls[i].index`）。
    pub index: usize,
    /// 首个非空分片提供的 id。
    pub id: String,
    /// 首个非空分片提供的函数名。
    pub name: String,
    /// 跨分片拼接的 arguments JSON 字符串。
    pub arguments: String,
}

/// 单个 SSE chunk 的 JSON：`choices[0]` 是 `delta` 的载体。
///
/// `reasoning_content` 在 [`StreamDelta`] 中显式声明（serde 解析），但只经
/// [`ModelEvent::ReasoningDelta`] 事件"路过"显示——累加器（content /
/// tool_calls）永不吸收它，从结构上保证推理过程不进入对话历史。
#[derive(Debug, Deserialize)]
struct StreamChunk {
    choices: Vec<StreamChoice>,
}

#[derive(Debug, Deserialize)]
struct StreamChoice {
    #[serde(default)]
    delta: StreamDelta,
    #[serde(default)]
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize, Default)]
struct StreamDelta {
    #[serde(default)]
    content: Option<String>,
    #[serde(default)]
    tool_calls: Vec<StreamDeltaToolCall>,
    /// 模型可选的推理过程增量。**只经事件流出，不进入累加器**。
    #[serde(default)]
    reasoning_content: Option<String>,
}

#[derive(Debug, Deserialize)]
struct StreamDeltaToolCall {
    #[serde(default)]
    index: usize,
    #[serde(default)]
    id: Option<String>,
    #[serde(default)]
    function: Option<StreamDeltaFunction>,
}

#[derive(Debug, Deserialize, Default)]
struct StreamDeltaFunction {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    arguments: Option<String>,
}

/// 处理 SSE 事件流中的一行，返回 `true` 表示流已结束，调用方应停止读取。
///
/// - 空行 / 注释行（`:` 开头）/ 非 `data:` 行 → 忽略，继续。
/// - `data: [DONE]` → 流结束。
/// - `data: <json>` → 解析 chunk：文本增量经 `TextDelta` 发出并累加，
///   推理增量经 `ReasoningDelta` 发出（**不落历史**），工具调用按 index
///   累加。`finish_reason` 为 `"tool_calls"` 时流结束（结果将组装为
///   [`Response::ToolCall`]）。
///
/// 纯函数：不依赖 reqwest / 网络，可独立构造事件流测试。
fn parse_sse_line(
    line: &str,
    acc: &mut StreamAccumulator,
    on_event: &mut dyn FnMut(ModelEvent),
) -> bool {
    let line = line.trim_end_matches(['\r', '\n']);
    if line.is_empty() || line.starts_with(':') {
        return false;
    }
    let Some(data) = line.strip_prefix("data:") else {
        return false;
    };
    let data = data.trim_start();
    if data == "[DONE]" {
        return true;
    }
    accumulate_chunk(data, acc, on_event)
}

/// 解析并累加一个 chunk 的 JSON（已剥掉 `data: ` 前缀）。
/// 返回 `true` 表示该 chunk 以 `finish_reason="tool_calls"` 结束流。
fn accumulate_chunk(
    json: &str,
    acc: &mut StreamAccumulator,
    on_event: &mut dyn FnMut(ModelEvent),
) -> bool {
    let chunk: StreamChunk = match serde_json::from_str(json) {
        Ok(chunk) => chunk,
        // 无法解析的行跳过，不中断整个流
        Err(_) => return false,
    };
    let Some(choice) = chunk.choices.first() else {
        return false;
    };

    // 文本增量：追加到 content，并经 on_event 发出（供 UI 逐字显示）。
    if let Some(text) = &choice.delta.content
        && !text.is_empty()
    {
        acc.content.push_str(text);
        on_event(ModelEvent::TextDelta(text.clone()));
    }

    // 推理过程增量：经 on_event 发出（供 UI 展示），**绝不追加到 content、
    // 不进任何累加器**——只是"路过"显示，对话历史不含推理（历史行为零变化）。
    if let Some(reasoning) = &choice.delta.reasoning_content
        && !reasoning.is_empty()
    {
        on_event(ModelEvent::ReasoningDelta(reasoning.clone()));
    }

    // 工具调用分片：按 index 找到已有分片，id/name 取首个非空，arguments 拼接。
    for tc in &choice.delta.tool_calls {
        let slot = match acc
            .tool_calls
            .iter()
            .position(|c| c.index == tc.index)
        {
            Some(pos) => &mut acc.tool_calls[pos],
            None => {
                acc.tool_calls.push(StreamToolCall {
                    index: tc.index,
                    id: String::new(),
                    name: String::new(),
                    arguments: String::new(),
                });
                acc.tool_calls
                    .last_mut()
                    .expect("just pushed")
            }
        };
        if let Some(id) = &tc.id
            && !id.is_empty()
            && slot.id.is_empty()
        {
            slot.id = id.to_string();
        }
        if let Some(name) = &tc
            .function
            .as_ref()
            .and_then(|f| f.name.as_ref())
            && !name.is_empty()
            && slot.name.is_empty()
        {
            slot.name = name.to_string();
        }
        if let Some(args) = &tc
            .function
            .as_ref()
            .and_then(|f| f.arguments.as_ref())
        {
            slot.arguments.push_str(args);
        }
    }

    if choice.finish_reason.as_deref() == Some("tool_calls") {
        return true;
    }
    false
}

/// 流结束后把累加结果转成 [`Response`]（与既有非流式语义一致：工具调用优先）。
fn stream_to_response(acc: &StreamAccumulator) -> Response {
    if !acc.tool_calls.is_empty() {
        let calls = acc
            .tool_calls
            .iter()
            .map(|tc| ToolCall {
                id: tc.id.clone(),
                name: tc.name.clone(),
                arguments: stream_arguments_value(&tc.arguments),
            })
            .collect();
        Response::ToolCall(calls)
    } else {
        Response::Text(acc.content.clone())
    }
}

/// 把拼接后的 arguments JSON 字符串解析为 [`serde_json::Value`]。
///
/// 空串按 `{}` 处理（部分模型不发送 arguments）；解析失败回退 `Null`，
/// 与既有非流式解析行为一致。
fn stream_arguments_value(raw: &str) -> serde_json::Value {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str(trimmed).unwrap_or(serde_json::Value::Null)
    }
}

impl OpenAICompatibleModel {
    pub fn new(api_key: String) -> Self {
        let base_url =
            env::var("OPENAI_BASE_URL").unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let model = env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

        Self::new_with_base_url(api_key, base_url, model)
    }

    /// 测试友好的构造方式：显式指定 base URL / model，不读取环境变量。
    ///
    /// 仅用于集成测试指向本地 mock server；`new` 仍从环境变量读取。
    pub fn new_with_base_url(api_key: String, base_url: String, model: String) -> Self {
        Self {
            api_key,
            base_url,
            model,
        }
    }
}

/// 连接阶段超时（共用）：连接建立挂死 10s 即报错，不无限等待。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
/// 每次读取的空闲超时（共用语义）：连接活着但 120s 内没有任何数据字节即报错。
/// 收到任意字节就重置计时——SSE 持续输出时永远不会触发，只拦截"静默挂起"。
/// 注：reqwest 0.13.4 的 **blocking** `ClientBuilder` 未暴露 `read_timeout`
/// （async 版本才有）；流式读取在 `complete_streaming` 里用
/// 「读取线程 + `recv_timeout`」实现完全相同的语义，见下。
const READ_TIMEOUT: Duration = Duration::from_secs(120);
/// 非流式请求整体超时：一次性请求-响应，120s 覆盖慢推理，挂死则失败。
const COMPLETE_TIMEOUT: Duration = Duration::from_secs(120);

/// 构造 blocking HTTP 客户端（`complete` / `complete_streaming` 共用）。
///
/// 超时策略：
/// - 共用 `connect_timeout(10s)`——连接建立阶段挂死不无限等；
/// - 非流式 `complete` 传入 `overall_timeout`（整体请求上限 120s，
///   对"连接活着但无数据"的场景已构成兜底，不会无限等待）；
/// - 流式 `complete_streaming` 传 `None`——SSE 是长连接，**绝不设整体
///   timeout**，整体 timeout 会按总时长误杀长推理（streaming 的设计就是
///   靠持续字节保活）；每次读取的空闲超时（`READ_TIMEOUT`）在读取线程侧实现。
///
/// `build()` 失败（如 rustls 初始化）经 `?` 传播，不吞错。
fn blocking_client(
    overall_timeout: Option<Duration>,
) -> Result<reqwest::blocking::Client, Box<dyn std::error::Error>> {
    let mut builder = reqwest::blocking::Client::builder().connect_timeout(CONNECT_TIMEOUT);

    // SOCKS 代理兼容：reqwest 未启用 `socks` feature（零新依赖 + 控制体积），
    // 环境里若有 socks*:// 代理（Clash/mihomo mixed-port 常见）会直接报
    // "unsupported scheme socks5"。检测到时显式设置归一化为 http:// 的代理
    // （mixed-port 同端口也讲 HTTP CONNECT）；显式 .proxy() 会关闭 system-proxy
    // 的自动读取。无 socks 时完全不触碰，保留 reqwest 原生 http(s) 代理 + NO_PROXY。
    let env_first = |keys: &[&str]| {
        keys.iter()
            .filter_map(|k| std::env::var(k).ok())
            .map(|v| v.trim().to_string())
            .find(|v| !v.is_empty())
    };
    let https_v = env_first(&["https_proxy", "HTTPS_PROXY"]);
    let http_v = env_first(&["http_proxy", "HTTP_PROXY"]);
    let all_v = env_first(&["all_proxy", "ALL_PROXY"]);
    if let Some((https_p, http_p)) =
        socks_proxy_override(https_v.as_deref(), http_v.as_deref(), all_v.as_deref())
    {
        if let Some(u) = https_p {
            builder = builder.proxy(reqwest::Proxy::https(&u)?);
        }
        if let Some(u) = http_p {
            builder = builder.proxy(reqwest::Proxy::http(&u)?);
        }
    }

    let builder = match overall_timeout {
        Some(timeout) => builder.timeout(timeout),
        None => builder,
    };
    Ok(builder.build()?)
}

/// reqwest 未启用 `socks` feature，遇到 `socks5://` / `socks5h://` / `socks4://`
/// 代理会报 "unsupported scheme socks5"。Clash / mihomo 的混合端口（mixed-port，
/// 常见 7890）在同一端口同时讲 HTTP 与 SOCKS，故把 socks* scheme 改写为
/// `http://`（authority/端口原样保留），让 reqwest 以 HTTP CONNECT 走同一端口。
/// 非 socks scheme、无 `://` 的值原样返回。
fn normalize_socks_proxy(url: &str) -> String {
    if let Some(idx) = url.find("://")
        && url[..idx].starts_with("socks")
    {
        return format!("http://{}", &url[idx + 3..]);
    }
    url.to_string()
}

/// 决策：当环境里存在 reqwest 不支持的 socks* 代理时，返回需要**显式**设置到
/// `ClientBuilder` 的归一化代理 `(https 代理, http 代理)`；否则返回 `None`
/// （全是 http(s) 代理或无代理——交给 reqwest system-proxy 原生处理，保留
/// `NO_PROXY` 语义，行为零变化）。
///
/// 入参为已按优先级解析好的环境变量值：`https` = https_proxy/HTTPS_PROXY，
/// `http` = http_proxy/HTTP_PROXY，`all` = all_proxy/ALL_PROXY（前两者的回退）。
fn socks_proxy_override(
    https: Option<&str>,
    http: Option<&str>,
    all: Option<&str>,
) -> Option<(Option<String>, Option<String>)> {
    let https_raw = https.or(all);
    let http_raw = http.or(all);
    let any_socks = [https_raw, http_raw]
        .iter()
        .flatten()
        .any(|u| {
            u.split("://")
                .next()
                .is_some_and(|s| s.starts_with("socks"))
        });
    if !any_socks {
        return None;
    }
    Some((
        https_raw.map(normalize_socks_proxy),
        http_raw.map(normalize_socks_proxy),
    ))
}

/// 有界回收流式读取线程：正常路径下服务端通常随 `[DONE]` 终止 body，线程在
/// body EOF 后立即退出，这里短等回收；若线程仍阻塞在读取上（服务端挂住 body /
/// 静默代理），超时后直接 drop `JoinHandle`（detach），不阻塞主线程——连接关闭
/// 后该线程会自行退出。
fn reap_reader(reader: std::thread::JoinHandle<()>) {
    let deadline = std::time::Instant::now() + Duration::from_millis(500);
    while !reader.is_finished() && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // 未在期限内退出 → drop JoinHandle，线程 detach
}

impl Model for OpenAICompatibleModel {
    fn complete(&self, messages: &[Message]) -> Result<Response, Box<dyn std::error::Error>> {
        // 非流式：一次性请求-响应。共用连接超时 10s，外加整体超时 120s
        // （覆盖慢推理与"连接活着但无数据"，不会无限等待）。build() 失败经 `?` 传播。
        let client = blocking_client(Some(COMPLETE_TIMEOUT))?;

        let request = ChatRequest {
            model: self.model.clone(),
            messages: messages
                .iter()
                .map(ApiMessage::from_core)
                .collect(),
            tools: tool::all_definitions()
                .into_iter()
                .map(|def| ApiTool {
                    kind: "function".to_string(),
                    function: ApiFunction {
                        name: def.name.to_string(),
                        description: def.description.to_string(),
                        parameters: def.parameters,
                    },
                })
                .collect(),
            stream: false,
        };

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(format!("API error: {status} body={body}").into());
        }

        let response: ChatResponse = response.json()?;

        let message = &response
            .choices
            .first()
            .ok_or("model returned no choices")?
            .message;

        // 有工具调用优先处理；content 为 null 时也能正确区分
        if let Some(tool_calls) = message
            .tool_calls
            .as_ref()
            .filter(|calls| !calls.is_empty())
        {
            let calls = tool_calls
                .iter()
                .map(|tc| ToolCall {
                    id: tc.id.clone(),
                    name: tc.function.name.clone(),
                    arguments: serde_json::from_str(&tc.function.arguments)
                        .unwrap_or(serde_json::Value::Null),
                })
                .collect();
            return Ok(Response::ToolCall(calls));
        }

        Ok(Response::Text(message.content.clone().unwrap_or_default()))
    }

    /// SSE 流式实现：请求带 `stream: true`，逐行读取 `text/event-stream`，
    /// 文本增量实时经 `on_event` 发出（根治长请求被代理空闲超时掐断）。
    ///
    /// 只处理 `data: ` 前缀的行；`data: [DONE]` 结束流。`reasoning_content`
    /// 不进入累加器（不追加 content、不进对话历史），只经 `ReasoningDelta`
    /// 事件"路过"显示，由 UI 决定是否打印。工具调用按 index 跨分片累加，
    /// 流结束后组装为 [`Response`]。
    ///
    /// 读取空闲超时（`READ_TIMEOUT`，120s）：reqwest blocking 0.13.4 未暴露
    /// `read_timeout`，因此把阻塞读放到独立线程、行经通道送回主线程，主线程
    /// `recv_timeout` 每次收一行即重置——语义与 `read_timeout` 一致：持续输出
    /// 永不触发，只有"连接活着但 120s 无任何数据"才报错（不会误杀长推理）。
    fn complete_streaming(
        &self,
        messages: &[Message],
        on_event: &mut dyn FnMut(ModelEvent),
    ) -> Result<Response, Box<dyn std::error::Error>> {
        // 流式（SSE 长连接）：**只设连接超时 10s，绝不设整体 timeout**——
        // 整体 timeout 会按总时长误杀长推理；streaming 的设计就是靠持续
        // 字节保活。空闲超时由下方读取线程 + recv_timeout 兜底。
        let client = blocking_client(None)?;

        let request = ChatRequest {
            model: self.model.clone(),
            messages: messages
                .iter()
                .map(ApiMessage::from_core)
                .collect(),
            tools: tool::all_definitions()
                .into_iter()
                .map(|def| ApiTool {
                    kind: "function".to_string(),
                    function: ApiFunction {
                        name: def.name.to_string(),
                        description: def.description.to_string(),
                        parameters: def.parameters,
                    },
                })
                .collect(),
            stream: true,
        };

        let response = client
            .post(format!("{}/chat/completions", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&request)
            .send()?;

        // 先检查 HTTP 状态码（非 2xx 时读取 body 报错，流式下错误体也可能是 JSON）。
        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .unwrap_or_else(|_| "<unreadable body>".to_string());
            return Err(format!("API error: {status} body={body}").into());
        }

        // 读取线程：把 blocking 的逐行读放到独立线程（response 为 Send），行经
        // 通道送回主线程。主线程 `recv_timeout(READ_TIMEOUT)` 实现每次读取的
        // 空闲超时——收到任意一行就重置，只有"连接活着但 120s 无任何数据"才报错。
        let (tx, rx) = std::sync::mpsc::channel::<Result<String, std::io::Error>>();
        let reader = std::thread::spawn(move || {
            let mut lines = std::io::BufReader::new(response).lines();
            loop {
                match lines.next() {
                    Some(Ok(line)) => {
                        if tx.send(Ok(line)).is_err() {
                            break; // 主线程已结束读取（[DONE] / 超时），停止发送
                        }
                    }
                    Some(Err(err)) => {
                        let _ = tx.send(Err(err));
                        break;
                    }
                    None => break, // body EOF：正常流结束
                }
            }
        });

        let mut acc = StreamAccumulator::default();
        let mut idle_timeout = false;
        loop {
            match rx.recv_timeout(READ_TIMEOUT) {
                // 收到一行：解析 / 累加 / 发事件；同时重置空闲计时。
                Ok(Ok(line)) => {
                    if parse_sse_line(&line, &mut acc, on_event) {
                        break; // [DONE] 或 finish_reason=tool_calls
                    }
                }
                // 底层读取错误（连接中断等）：原样传播，不特殊包装。
                Ok(Err(err)) => {
                    drop(rx);
                    reap_reader(reader);
                    return Err(err.into());
                }
                // 连接活着但 120s 无数据：空闲超时（持续输出时每行都重置，不会误杀）。
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                    idle_timeout = true;
                    break;
                }
                // 读取线程已退出（body EOF）且行已消费完：流正常结束。
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }
        drop(rx);
        // 有界回收读取线程（正常路径 body EOF 后线程立即退出）；若仍阻塞在读取上
        // （服务端挂住 body / 静默代理）则 detach，不阻塞主线程。
        reap_reader(reader);
        if idle_timeout {
            return Err(format!(
                "streaming response idle: no data for {}s (read timeout)",
                READ_TIMEOUT.as_secs()
            )
            .into());
        }
        Ok(stream_to_response(&acc))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chat_request_serializes_to_expected_shape() {
        let request = ChatRequest {
            model: "gpt-5.6-sol".to_string(),
            messages: vec![ApiMessage::from_core(&Message::user("hello"))],
            tools: vec![],
            stream: false,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["model"], "gpt-5.6-sol");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hello");
    }

    #[test]
    fn api_message_converts_core_message() {
        let api = ApiMessage::from_core(&Message::assistant("hi"));
        assert_eq!(api.role, "assistant");
        assert_eq!(api.content.as_deref(), Some("hi"));
        assert!(api.tool_call_id.is_none());
        assert!(api.tool_calls.is_empty());
    }

    #[test]
    fn api_message_converts_tool_result() {
        let api = ApiMessage::from_core(&Message::tool("file contents", "call_123"));
        assert_eq!(api.role, "tool");
        assert_eq!(api.content.as_deref(), Some("file contents"));
        assert_eq!(api.tool_call_id.as_deref(), Some("call_123"));
        assert!(api.tool_calls.is_empty());
    }

    #[test]
    fn api_message_converts_assistant_tool_call() {
        let call = ToolCall {
            id: "call_1".to_string(),
            name: "read_file".to_string(),
            arguments: serde_json::json!({ "path": "src/main.rs" }),
        };
        let api = ApiMessage::from_core(&Message::assistant_tool_calls(vec![call]));

        assert_eq!(api.role, "assistant");
        assert!(api.content.is_none());
        assert_eq!(api.tool_calls.len(), 1);
        assert_eq!(api.tool_calls[0].id, "call_1");
        assert_eq!(
            api.tool_calls[0].function.arguments,
            r#"{"path":"src/main.rs"}"#
        );
    }

    #[test]
    fn chat_request_serializes_tools() {
        let request = ChatRequest {
            model: "m".to_string(),
            messages: vec![],
            tools: vec![ApiTool {
                kind: "function".to_string(),
                function: ApiFunction {
                    name: "read_file".to_string(),
                    description: "Read a file".to_string(),
                    parameters: serde_json::json!({ "type": "object" }),
                },
            }],
            stream: false,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["tools"][0]["type"], "function");
        assert_eq!(json["tools"][0]["function"]["name"], "read_file");
    }

    #[test]
    fn chat_response_deserializes_text() {
        let json = r#"{
            "choices": [ { "message": { "content": "hello back" } } ]
        }"#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        assert_eq!(
            response.choices[0]
                .message
                .content
                .as_deref(),
            Some("hello back")
        );
    }

    #[test]
    fn chat_response_deserializes_tool_call() {
        let json = r#"{
            "choices": [ {
                "message": {
                    "content": null,
                    "tool_calls": [{
                        "id": "call_9",
                        "type": "function",
                        "function": {
                            "name": "read_file",
                            "arguments": "{\"path\": \"src/main.rs\"}"
                        }
                    }]
                }
            } ]
        }"#;

        let response: ChatResponse = serde_json::from_str(json).unwrap();
        let calls = response.choices[0]
            .message
            .tool_calls
            .as_ref()
            .unwrap();
        assert_eq!(calls[0].id, "call_9");
        assert_eq!(calls[0].function.name, "read_file");
        assert_eq!(calls[0].function.arguments, r#"{"path": "src/main.rs"}"#);
    }

    // ---------------- SSE 流式解析：纯函数测试 ----------------

    /// 构造一行 `data: <json>` 事件（保持真实 SSE 形态）。
    fn sse_data(delta: serde_json::Value, finish_reason: Option<&str>) -> String {
        let json = serde_json::json!({
            "choices": [{ "delta": delta, "finish_reason": finish_reason }]
        });
        format!("data: {json}")
    }

    /// 把一段 SSE 事件流逐行喂给解析器，收集结束标记、文本增量与推理增量。
    fn run_sse(stream: &str) -> (StreamAccumulator, Vec<String>, Vec<String>, bool) {
        let mut acc = StreamAccumulator::default();
        let mut deltas = Vec::new();
        let mut reasoning = Vec::new();
        let mut ended = false;
        for line in stream.lines() {
            if parse_sse_line(line, &mut acc, &mut |event| match event {
                ModelEvent::TextDelta(text) => deltas.push(text),
                ModelEvent::ReasoningDelta(text) => reasoning.push(text),
            }) {
                ended = true;
                break;
            }
        }
        (acc, deltas, reasoning, ended)
    }

    /// 真实形态的文本事件流：多段 delta + 空行 + 注释行 + 夹杂 reasoning_content + [DONE]。
    ///
    /// 断言：
    /// (a) 文本跨 delta 正确累加，reasoning_content 不进入 content；
    /// (b) reasoning 经 `ReasoningDelta` 事件"路过"显示（内容正确），
    ///     最终 [`Response`] 是纯文本，不含任何推理；
    /// 空行 / 注释行被忽略，[DONE] 结束流。
    #[test]
    fn sse_text_stream_accumulates_and_drops_reasoning() {
        let stream = "\
data: {\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}
\n: this is a comment\n\ndata: {\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking hard...\"},\"finish_reason\":null}]}
\ndata: {\"choices\":[{\"delta\":{\"content\":\" world\"},\"finish_reason\":null}]}
\ndata: {\"choices\":[{\"delta\":{\"reasoning_content\":\"more reasoning\"},\"finish_reason\":null}]}
\ndata: {\"choices\":[{\"delta\":{\"content\":\"!\"},\"finish_reason\":\"stop\"}]}
\ndata: [DONE]\n";

        let (acc, deltas, reasoning, ended) = run_sse(stream);

        assert!(ended, "[DONE] should end the stream");
        // (a) 文本正确累加，reasoning 不进入 content
        assert_eq!(acc.content, "Hello world!");
        // 每个文本 delta 都经 on_event 发出，供 UI 增量显示
        assert_eq!(deltas, vec!["Hello", " world", "!"]);
        // (b) reasoning 经 ReasoningDelta 事件"路过"，内容正确
        assert_eq!(reasoning, vec!["thinking hard...", "more reasoning"]);
        // reasoning 绝不进入工具调用累加器
        assert!(acc.tool_calls.is_empty());
        // 最终 Response 是纯文本，不含任何推理
        match stream_to_response(&acc) {
            Response::Text(text) => assert_eq!(text, "Hello world!"),
            other => panic!("expected Text, got {other:?}"),
        }
    }

    /// 两个工具调用的 arguments 跨多个分片，夹杂 reasoning_content，
    /// 以 `finish_reason="tool_calls"` 结束。
    ///
    /// 断言：
    /// (b) 两个工具调用的 id/name 取首个非空分片，arguments JSON 正确拼接解析；
    /// (c) 流结束后得到 [`Response::ToolCall`]。
    #[test]
    fn sse_tool_call_stream_accumulates_fragments() {
        let lines = vec![
            // call 0：首个分片带 id/name，arguments 只给一半
            sse_data(
                serde_json::json!({"tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": "{\"path\":\"a" }
                }]}),
                None,
            ),
            "".to_string(),
            ": another comment".to_string(),
            // call 0：后续分片只给 arguments 剩余一半（id/name 为 null）
            sse_data(
                serde_json::json!({"tool_calls": [{
                    "index": 0,
                    "function": { "arguments": ".txt\",\"content\":\"1\"}" }
                }]}),
                None,
            ),
            // call 1：完整单分片
            sse_data(
                serde_json::json!({"tool_calls": [{
                    "index": 1,
                    "id": "call_2",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": "{\"path\":\"b.txt\",\"content\":\"2\"}" }
                }]}),
                None,
            ),
            // 夹杂的 reasoning_content：应被忽略
            sse_data(
                serde_json::json!({"reasoning_content": "deciding which tool..."}),
                None,
            ),
            // 以 finish_reason=tool_calls 结束流
            sse_data(serde_json::json!({}), Some("tool_calls")),
            "data: [DONE]".to_string(),
        ];

        let mut acc = StreamAccumulator::default();
        let mut deltas = Vec::new();
        let mut reasoning = Vec::new();
        let mut ended = false;
        for line in &lines {
            if parse_sse_line(line, &mut acc, &mut |event| match event {
                ModelEvent::TextDelta(text) => deltas.push(text),
                ModelEvent::ReasoningDelta(text) => reasoning.push(text),
            }) {
                ended = true;
                break;
            }
        }

        assert!(ended, "finish_reason=tool_calls should end the stream");
        // 工具调用过程不产生任何文本增量
        assert!(deltas.is_empty());
        // 夹杂的 reasoning 经事件"路过"（内容正确，不落历史）
        assert_eq!(reasoning, vec!["deciding which tool..."]);

        // (b) 两个工具调用按 index 累加，id/name 取首个非空分片
        assert_eq!(acc.tool_calls.len(), 2);
        assert_eq!(acc.tool_calls[0].id, "call_1");
        assert_eq!(acc.tool_calls[0].name, "write_file");
        assert_eq!(
            acc.tool_calls[0].arguments,
            r#"{"path":"a.txt","content":"1"}"#
        );
        assert_eq!(acc.tool_calls[1].id, "call_2");
        assert_eq!(acc.tool_calls[1].name, "write_file");
        assert_eq!(
            acc.tool_calls[1].arguments,
            r#"{"path":"b.txt","content":"2"}"#
        );

        // (c) finish_reason=tool_calls → Response::ToolCall，arguments 解析为 Value
        let response = stream_to_response(&acc);
        match response {
            Response::ToolCall(calls) => {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].id, "call_1");
                assert_eq!(calls[0].name, "write_file");
                assert_eq!(
                    calls[0].arguments,
                    serde_json::json!({"path": "a.txt", "content": "1"})
                );
                assert_eq!(calls[1].id, "call_2");
                assert_eq!(
                    calls[1].arguments,
                    serde_json::json!({"path": "b.txt", "content": "2"})
                );
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    /// 空 arguments（模型未发送）按 `{}` 处理，与既有非流式语义兼容。
    #[test]
    fn sse_empty_arguments_defaults_to_empty_object() {
        let lines = vec![
            sse_data(
                serde_json::json!({"tool_calls": [{
                    "index": 0,
                    "id": "call_1",
                    "type": "function",
                    "function": { "name": "write_file", "arguments": "" }
                }]}),
                Some("tool_calls"),
            ),
            "data: [DONE]".to_string(),
        ];

        let mut acc = StreamAccumulator::default();
        for line in &lines {
            if parse_sse_line(line, &mut acc, &mut |_| {}) {
                break;
            }
        }

        let response = stream_to_response(&acc);
        match response {
            Response::ToolCall(calls) => {
                assert_eq!(calls.len(), 1);
                assert_eq!(calls[0].arguments, serde_json::json!({}));
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
    }

    // ---------------- mock HTTP server + 集成测试 ----------------

    use std::io::{Read, Write};
    use std::net::TcpListener;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::mpsc;
    use std::thread;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    /// 测试用临时目录（与 agent/session 测试风格一致，避免污染真实项目）。
    fn temp_root() -> PathBuf {
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir =
            std::env::temp_dir().join(format!("karakuri-model-test-{}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// 每个测试一个独立的 mock server：监听 127.0.0.1:0（OS 分配端口）。
    ///
    /// `handler` 接收请求 body，返回响应 body（按需区分第一次/第二次请求）。
    /// 每次请求的原始 body 通过 channel 送出，供测试断言。
    ///
    /// server 线程使用非阻塞 accept，`Drop` 时设置停止标志并 join，
    /// 避免无限循环线程导致测试挂起。
    struct MockServer {
        addr: String,
        requests: mpsc::Receiver<String>,
        stop: std::sync::Arc<std::sync::atomic::AtomicBool>,
        join: Option<thread::JoinHandle<()>>,
    }

    impl MockServer {
        fn start(handler: impl Fn(usize, &str) -> (u16, String) + Send + 'static) -> Self {
            let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock server");
            let addr = listener.local_addr().unwrap().to_string();
            listener
                .set_nonblocking(true)
                .expect("set nonblocking");
            let (tx, rx) = mpsc::channel();
            let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            let stop_flag = stop.clone();

            let join = thread::spawn(move || {
                let mut idx = 0usize;
                while !stop_flag.load(std::sync::atomic::Ordering::SeqCst) {
                    let mut stream = match listener.accept() {
                        Ok((s, _)) => s,
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            thread::sleep(std::time::Duration::from_millis(5));
                            continue;
                        }
                        Err(_) => break,
                    };
                    let _ = stream.set_nonblocking(false);

                    // 读取请求头 + body（Content-Length 分帧）
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 4096];
                    let mut header_end = None;
                    let mut content_length = 0usize;
                    let mut sent_continue = false;

                    // 读直到拿到完整请求（头 + 指定长度 body）
                    loop {
                        let n = match stream.read(&mut chunk) {
                            Ok(0) => break,
                            Ok(n) => n,
                            Err(_) => break,
                        };
                        buf.extend_from_slice(&chunk[..n]);

                        if header_end.is_none()
                            && let Some(pos) = find_header_end(&buf)
                        {
                            header_end = Some(pos);
                            content_length = parse_content_length(&buf[..pos]);

                            // reqwest 对 >1KB body 会发 `Expect: 100-continue`，
                            // 必须先回 100 才能收到 body，否则死锁。
                            let headers = String::from_utf8_lossy(&buf[..pos]).to_ascii_lowercase();
                            if headers.contains("expect: 100-continue") && !sent_continue {
                                let _ = stream.write_all(b"HTTP/1.1 100 Continue\r\n\r\n");
                                let _ = stream.flush();
                                sent_continue = true;
                            }
                        }

                        // 已收到完整 body
                        if let Some(pos) = header_end
                            && buf.len() >= pos + content_length
                        {
                            break;
                        }
                    }

                    // 分离 body
                    let body = if let Some(pos) = header_end {
                        let body_bytes = buf[pos..]
                            .get(..content_length.min(buf.len() - pos))
                            .unwrap_or(&buf[pos..]);
                        String::from_utf8_lossy(body_bytes).to_string()
                    } else {
                        String::new()
                    };

                    // 发送请求 body 供断言
                    let _ = tx.send(body.clone());

                    // 生成响应
                    let (status, response_body) = handler(idx, &body);
                    idx += 1;
                    let response = format!(
                        "HTTP/1.1 {status} \r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                        response_body.len(),
                        response_body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                }
            });

            Self {
                addr,
                requests: rx,
                stop,
                join: Some(join),
            }
        }

        fn base_url(&self) -> String {
            format!("http://{}", self.addr)
        }

        /// 阻塞等待第 n 次请求的 body（1-indexed）。
        fn request_body(&self, n: usize) -> String {
            self.requests
                .iter()
                .nth(n - 1)
                .unwrap_or_default()
        }
    }

    impl Drop for MockServer {
        fn drop(&mut self) {
            // 停止 server 线程并 join，避免挂起
            self.stop
                .store(true, std::sync::atomic::Ordering::SeqCst);
            if let Some(join) = self.join.take() {
                let _ = join.join();
            }
        }
    }

    fn find_header_end(buf: &[u8]) -> Option<usize> {
        buf.windows(4)
            .position(|w| w == b"\r\n\r\n")
            .map(|pos| pos + 4)
    }

    fn parse_content_length(header: &[u8]) -> usize {
        let text = String::from_utf8_lossy(header);
        text.lines()
            .find_map(|line| {
                let lower = line.to_ascii_lowercase();
                lower
                    .strip_prefix("content-length:")
                    .map(|v| v.trim().parse().unwrap_or(0))
            })
            .unwrap_or(0)
    }

    /// 完整 Agent Loop 用 mock server 跑通：
    ///
    /// ```text
    /// Agent::run_turn
    ///   → OpenAICompatibleModel::complete_streaming (base_url → mock)
    ///   → HTTP POST /chat/completions（stream: true）
    ///   → mock SSE（text/event-stream 逐行）
    ///   → Agent 解析 → 执行 → 回传 → 最终回答
    /// ```
    #[test]
    fn agent_plain_text_through_mock_server() {
        let mock = MockServer::start(|_idx, _body| {
            (
                200,
                "data: {\"choices\":[{\"delta\":{\"content\":\"hello back\"},\"finish_reason\":\"stop\"}]}\n\ndata: [DONE]\n"
                    .to_string(),
            )
        });
        let model = OpenAICompatibleModel::new_with_base_url(
            "test-key".to_string(),
            mock.base_url(),
            "gpt-test".to_string(),
        );
        let agent = crate::agent::Agent::new_with_root_for_test(model, temp_root());

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "hi")
            .unwrap();

        // 最终回答
        assert_eq!(conversation.len(), 2);
        assert_eq!(conversation[0].content, "hi");
        assert_eq!(conversation[1].content, "hello back");

        // 请求层断言
        let body = mock.request_body(1);
        let json: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(json["model"], "gpt-test");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "hi");
        // run_turn 经 run_turn_streaming → 流式请求
        assert_eq!(json["stream"], true);
        // tools 定义应随请求发送
        assert!(json["tools"].is_array());
        assert!(json["tools"][0]["type"] == "function");
    }

    /// 最重要的测试：ToolCall → 工具执行 → Tool Result 回传 → 最终回答。
    ///
    /// mock server 区分第一次/第二次请求：
    /// - 第一次：返回 SSE tool_calls（read_file）
    /// - 第二次：返回 SSE 最终文本
    ///
    /// 并验证第二次请求的 messages 包含 assistant tool_call + tool result（正确 tool_call_id）。
    #[test]
    fn agent_tool_call_through_mock_server() {
        let root = temp_root();
        std::fs::write(root.join("note.txt"), "important data").unwrap();

        let mock = MockServer::start(|idx, _body| {
            if idx == 0 {
                // 第一次：SSE tool_call（arguments 跨两个分片，验证流式拼接）
                // 片段 1：id/name + arguments 前半 `{"path": "note`
                let chunk1 = serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "id": "call_abc",
                                "type": "function",
                                "function": {
                                    "name": "read_file",
                                    "arguments": "{\"path\": \"note"
                                }
                            }]
                        },
                        "finish_reason": null
                    }]
                });
                // 片段 2：arguments 后半 `.txt"}`，finish_reason=tool_calls
                let chunk2 = serde_json::json!({
                    "choices": [{
                        "delta": {
                            "tool_calls": [{
                                "index": 0,
                                "function": { "arguments": ".txt\"}" }
                            }]
                        },
                        "finish_reason": "tool_calls"
                    }]
                });
                (
                    200,
                    format!("data: {chunk1}\n\ndata: {chunk2}\n\ndata: [DONE]\n"),
                )
            } else {
                // 第二次：SSE final text
                let chunk = serde_json::json!({
                    "choices": [{
                        "delta": {"content": "found the data" },
                        "finish_reason": "stop"
                    }]
                });
                (200, format!("data: {chunk}\n\ndata: [DONE]\n"))
            }
        });

        let model = OpenAICompatibleModel::new_with_base_url(
            "test-key".to_string(),
            mock.base_url(),
            "gpt-test".to_string(),
        );
        let agent = crate::agent::Agent::new_with_root_for_test(model, root.clone());

        let mut conversation = Vec::new();
        agent
            .run_turn(&mut conversation, "read note.txt")
            .unwrap();

        // conversation: user, assistant(tool_calls), tool(result), assistant(final)
        assert_eq!(conversation.len(), 4);
        assert_eq!(conversation[3].content, "found the data");

        // 第二次请求的 messages 必须包含 assistant tool_call + tool result
        let second = mock.request_body(2);
        let json: serde_json::Value = serde_json::from_str(&second).unwrap();
        let messages = json["messages"].as_array().unwrap();
        assert_eq!(messages.len(), 3);

        // assistant tool_call
        assert_eq!(messages[1]["role"], "assistant");
        assert_eq!(messages[1]["tool_calls"][0]["id"], "call_abc");
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["name"],
            "read_file"
        );
        assert_eq!(
            messages[1]["tool_calls"][0]["function"]["arguments"],
            r#"{"path":"note.txt"}"#
        );

        // tool result
        assert_eq!(messages[2]["role"], "tool");
        assert_eq!(messages[2]["tool_call_id"], "call_abc");
        assert_eq!(messages[2]["content"], "important data");
    }

    /// API 错误（HTTP 500）必须传播为 `AgentError::Model`，不吞掉。
    #[test]
    fn agent_api_error_propagates_from_mock_server() {
        let mock = MockServer::start(|_idx, _body| (500, r#"{"error": "boom"}"#.to_string()));
        let model = OpenAICompatibleModel::new_with_base_url(
            "test-key".to_string(),
            mock.base_url(),
            "gpt-test".to_string(),
        );
        let agent = crate::agent::Agent::new_with_root_for_test(model, temp_root());

        let mut conversation = Vec::new();
        let result = agent.run_turn(&mut conversation, "hi");

        assert!(matches!(result, Err(crate::agent::AgentError::Model(_))));
        // 错误后 conversation 回滚：user 消息被移除
        assert!(conversation.is_empty());
    }

    /// normalize_socks_proxy：socks* scheme 改写为 http，其余原样。
    #[test]
    fn normalize_socks_proxy_rewrites_scheme_only() {
        assert_eq!(
            normalize_socks_proxy("socks5://127.0.0.1:7890"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_socks_proxy("socks5h://example.com:1080"),
            "http://example.com:1080"
        );
        assert_eq!(normalize_socks_proxy("socks4://h:1"), "http://h:1");
        // 非 socks scheme / 无 scheme 原样。
        assert_eq!(
            normalize_socks_proxy("http://127.0.0.1:7890"),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_socks_proxy("https://proxy.corp:3128"),
            "https://proxy.corp:3128"
        );
        assert_eq!(normalize_socks_proxy("127.0.0.1:7890"), "127.0.0.1:7890");
    }

    /// socks_proxy_override：仅当出现 socks* 代理时返回归一化结果，否则 None。
    #[test]
    fn socks_override_only_triggers_on_socks_scheme() {
        // https 走 socks、http 走 http（典型 Clash mixed-port）：两者归一化为 http。
        let r = socks_proxy_override(
            Some("socks5://127.0.0.1:7890"),
            Some("http://127.0.0.1:7890"),
            None,
        )
        .expect("socks present → override");
        assert_eq!(r.0.as_deref(), Some("http://127.0.0.1:7890"));
        assert_eq!(r.1.as_deref(), Some("http://127.0.0.1:7890"));

        // 仅 all_proxy 是 socks：http/https 都回退到它并归一化。
        let r = socks_proxy_override(None, None, Some("socks5h://h:1080")).expect("all socks");
        assert_eq!(r.0.as_deref(), Some("http://h:1080"));
        assert_eq!(r.1.as_deref(), Some("http://h:1080"));

        // 全是 http(s) 代理 → None（交给 reqwest system-proxy，保留 NO_PROXY）。
        assert_eq!(
            socks_proxy_override(Some("http://h:3128"), Some("http://h:3128"), None),
            None
        );
        // 无任何代理 → None。
        assert_eq!(socks_proxy_override(None, None, None), None);
    }
}
