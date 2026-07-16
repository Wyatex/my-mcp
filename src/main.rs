use axum::{
    extract::Query,
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use chrono::{Days, Duration, Local, Months};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::env;
use tokio::signal;
use tower_http::cors::{Any, CorsLayer};

// --- 数据结构 ---

#[derive(Deserialize)]
struct McpQuery {
    #[serde(rename = "apiKey")]
    api_key: String,
    model: Option<String>,
}

#[derive(Deserialize)]
struct RpcRequest {
    id: Option<serde_json::Value>,
    method: String,
    params: Option<serde_json::Value>,
}

#[derive(Serialize)]
struct RpcResponse {
    jsonrpc: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<RpcError>,
}

#[derive(Serialize)]
struct RpcError {
    code: i32,
    message: String,
}

impl RpcResponse {
    fn success(id: Option<serde_json::Value>, result: serde_json::Value) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: Some(result),
            error: None,
        }
    }
    fn error(id: Option<serde_json::Value>, code: i32, message: String) -> Self {
        Self {
            jsonrpc: "2.0",
            id,
            result: None,
            error: Some(RpcError { code, message }),
        }
    }
}

// --- Kimi API 请求逻辑 ---

async fn handle_search_tool_call(
    query: Option<&str>,
    api_key: &str,
    model: &str,
) -> Result<serde_json::Value, String> {
    let query_text = match query {
        Some(q) if !q.trim().is_empty() => q,
        _ => {
            return Ok(json!({
                "content": [{ "type": "text", "text": "请输入搜索关键词" }]
            }))
        }
    };

    println!("[Tool Call] search called with query: {}", query_text);

    let client = Client::new();

    // 1. 初始化对话历史 (messages)
    let mut messages = vec![
        json!({
            "role": "system",
            "content": "你是 Kimi，由 Moonshot AI 提供的人工智能助手，你更擅长中文和英文的对话。你会为用户提供安全，有帮助，准确的回答。同时，你会拒绝一切涉及恐怖主义，种族歧视，黄色暴力等问题的回答。Moonshot AI 为专有名词，不可翻译成其他语言。"
        }),
        json!({
            "role": "user",
            "content": query_text
        }),
    ];

    // 2. 声明内置联网搜索工具
    let tools = json!([{
        "type": "builtin_function",
        "function": {
            "name": "$web_search"
        }
    }]);

    // 3. 循环发起对话，直到模型输出纯文本响应
    loop {
        let request_body = json!({
            "model": model,
            "messages": messages,
            "tools": tools
        });

        let res = client
            .post("https://api.moonshot.cn/v1/chat/completions")
            .bearer_auth(api_key)
            .json(&request_body)
            .send()
            .await
            .map_err(|e| format!("Network error: {}", e))?;

        if !res.status().is_success() {
            let err_text = res.text().await.unwrap_or_default();
            return Err(format!("Moonshot API error: {}", err_text));
        }

        let res_json: serde_json::Value = res
            .json()
            .await
            .map_err(|e| format!("Failed to parse response: {}", e))?;

        let choice = &res_json["choices"][0];
        let message = &choice["message"];
        let finish_reason = choice["finish_reason"].as_str().unwrap_or("");

        // 4. 判断是否触发了工具调用
        if finish_reason == "tool_calls" || message.get("tool_calls").is_some() {
            println!("[Tool Call] Kimi triggered $web_search, passing context back to model...");

            // (1) 必须先把 assistant 的回复原样加进 messages 中
            messages.push(message.clone());

            // (2) 遍历所有的 tool_calls 构建 tool 角色消息返回给大模型
            if let Some(tool_calls) = message["tool_calls"].as_array() {
                for tool_call in tool_calls {
                    let tool_call_id = tool_call["id"].as_str().unwrap_or("");
                    let function_name = tool_call["function"]["name"].as_str().unwrap_or("");
                    let arguments_str = tool_call["function"]["arguments"].as_str().unwrap_or("{}");

                    // 将入参对象（本身已是 json 字符串）作为内容填入 role: tool 中
                    messages.push(json!({
                        "role": "tool",
                        "tool_call_id": tool_call_id,
                        "name": function_name,
                        "content": arguments_str
                    }));
                }
            }

            // 继续下一轮循环
            continue;
        }

        // 5. 如果不再是工具调用（通常 finish_reason == "stop"），获取最终的文本结果
        let text = message["content"]
            .as_str()
            .unwrap_or("无返回结果")
            .to_string();

        println!("[Tool Call] search returned final response.");

        return Ok(json!({
            "content": [{ "type": "text", "text": text }]
        }));
    }
}

// --- 时间相关工具逻辑 ---

fn handle_get_current_time() -> Result<serde_json::Value, String> {
    let now = Local::now().format("%Y-%m-%d %H:%M:%S").to_string();
    Ok(json!({
        "content": [{ "type": "text", "text": now }]
    }))
}

fn handle_time_calculator(args: &serde_json::Value) -> Result<serde_json::Value, String> {
    let operation = args.get("operation").and_then(|v| v.as_str()).unwrap_or("");
    let amount = args.get("amount").and_then(|v| v.as_i64()).unwrap_or(0);
    let unit = args.get("unit").and_then(|v| v.as_str()).unwrap_or("");

    let now = Local::now();
    let result = match operation {
        "add" => match unit {
            "seconds" => now.checked_add_signed(Duration::seconds(amount)),
            "minutes" => now.checked_add_signed(Duration::minutes(amount)),
            "hours" => now.checked_add_signed(Duration::hours(amount)),
            "days" => now.checked_add_days(Days::new(amount as u64)),
            "months" => now.checked_add_months(Months::new(amount as u32)),
            "years" => now.checked_add_months(Months::new((amount * 12) as u32)),
            _ => return Err(format!("Unsupported unit: {}", unit)),
        },
        "subtract" => match unit {
            "seconds" => now.checked_sub_signed(Duration::seconds(amount)),
            "minutes" => now.checked_sub_signed(Duration::minutes(amount)),
            "hours" => now.checked_sub_signed(Duration::hours(amount)),
            "days" => now.checked_sub_days(Days::new(amount as u64)),
            "months" => now.checked_sub_months(Months::new(amount as u32)),
            "years" => now.checked_sub_months(Months::new((amount * 12) as u32)),
            _ => return Err(format!("Unsupported unit: {}", unit)),
        },
        _ => return Err(format!("Unsupported operation: {}", operation)),
    };

    match result {
        Some(t) => Ok(json!({
            "content": [{ "type": "text", "text": t.format("%Y-%m-%d %H:%M:%S").to_string() }]
        })),
        None => Err("Time calculation error (overflow)".to_string()),
    }
}

// --- MCP 协议处理 ---

async fn process_message(req: RpcRequest, api_key: &str, model: &str) -> Option<RpcResponse> {
    match req.method.as_str() {
        // 1. 初始化握手
        "initialize" => Some(RpcResponse::success(
            req.id,
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities": { "tools": {} },
                "serverInfo": { "name": "kimi-search-mcp", "version": "1.0.0" }
            }),
        )),

        // 2. 客户端通知 (无返回值)
        "notifications/initialized" => None,

        // 3. 获取工具列表
        "tools/list" => Some(RpcResponse::success(
            req.id,
            json!({
                "tools": [
                    {
                        "name": "search",
                        "description": "AI联网搜索",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "query": { "type": "string", "description": "搜索内容" }
                            },
                            "required": ["query"]
                        }
                    },
                    {
                        "name": "get_current_time",
                        "description": "获取当前时间",
                        "inputSchema": {
                            "type": "object",
                            "properties": {}
                        }
                    },
                    {
                        "name": "time_calculator",
                        "description": "时间计算器",
                        "inputSchema": {
                            "type": "object",
                            "properties": {
                                "operation": { "type": "string", "enum": ["add", "subtract"], "description": "加减操作" },
                                "amount": { "type": "integer", "description": "数量" },
                                "unit": { "type": "string", "enum": ["seconds", "minutes", "hours", "days", "months", "years"], "description": "单位" }
                            },
                            "required": ["operation", "amount", "unit"]
                        }
                    }
                ]
            }),
        )),

        // 4. 调用工具
        "tools/call" => {
            let params = req.params.as_ref();
            let name = params.and_then(|p| p.get("name")).and_then(|n| n.as_str());
            let default_args = json!({});
            let arguments = params.and_then(|p| p.get("arguments")).unwrap_or(&default_args);

            let result = match name {
                Some("search") => {
                    let query = arguments.get("query").and_then(|q| q.as_str());
                    handle_search_tool_call(query, api_key, model).await
                }
                Some("get_current_time") => handle_get_current_time(),
                Some("time_calculator") => handle_time_calculator(arguments),
                _ => Err("Unknown tool".to_string()),
            };

            match result {
                Ok(res) => Some(RpcResponse::success(req.id, res)),
                Err(e) => Some(RpcResponse::error(req.id, -32603, e)),
            }
        }

        // 未知方法
        _ => Some(RpcResponse::error(
            req.id,
            -32601,
            "Method not found".to_string(),
        )),
    }
}

// --- HTTP 路由处理 ---

async fn mcp_post_handler(
    Query(query): Query<McpQuery>,
    Json(payload): Json<RpcRequest>,
) -> Response {
    let model = query
        .model
        .unwrap_or_else(|| "kimi-k2-turbo-preview".to_string());

    if let Some(response) = process_message(payload, &query.api_key, &model).await {
        // 正常返回 JSON
        Json(response).into_response()
    } else {
        // 返回 202 Accepted (Notification 规范)
        StatusCode::ACCEPTED.into_response()
    }
}

async fn mcp_get_handler() -> Response {
    (
        StatusCode::METHOD_NOT_ALLOWED,
        "SSE stream not supported at this endpoint",
    )
        .into_response()
}

fn parse_port() -> u16 {
    let args: Vec<String> = env::args().collect();
    let mut port = 3000u16;

    let mut i = 1;
    while i < args.len() {
        match args[i].as_str() {
            "-p" | "--port" => {
                if i + 1 < args.len() {
                    if let Ok(p) = args[i + 1].parse::<u16>() {
                        port = p;
                    } else {
                        eprintln!("Invalid port number: {}", args[i + 1]);
                        std::process::exit(1);
                    }
                    i += 2;
                } else {
                    eprintln!("Option -p requires a port number");
                    std::process::exit(1);
                }
            }
            _ => {
                eprintln!("Unknown option: {}", args[i]);
                eprintln!("Usage: {} [-p <port>] [--port <port>]", args[0]);
                std::process::exit(1);
            }
        }
    }
    port
}

#[tokio::main]
async fn main() {
    let port = parse_port();

    // 配置 CORS
    let cors = CorsLayer::new()
        .allow_origin(Any)
        .allow_methods([Method::GET, Method::POST, Method::OPTIONS])
        .allow_headers(Any);

    // 路由
    let app = Router::new()
        .route("/mcp", post(mcp_post_handler))
        .route("/mcp", get(mcp_get_handler))
        .layer(cors);

    let addr = format!("0.0.0.0:{}", port);
    println!("Server running on http://{}", addr);
    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap();
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}
/// 监听退出信号的函数
async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install signal handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => {
            println!("Received Ctrl+C (SIGINT), starting graceful shutdown...");
        },
        _ = terminate => {
            println!("Received Docker Stop (SIGTERM), starting graceful shutdown...");
        },
    }
}
