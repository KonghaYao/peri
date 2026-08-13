//! Web 产品主旅程：cookie auth → project/session create → server restart →
//! catalog 恢复 → 精确 session/load 重新激活。

mod common;

use std::time::Duration;

use acp_hub_proto::ack::AckStatus;
use acp_hub_proto::action::{
    ActionEnvelope, PersistedSessionCreatePayload, PersistedSessionOpenPayload,
    ProjectCreatePayload, PromptChatPayload,
};
use acp_hub_proto::Frame;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use common::{
    doc_from_snapshots, project_field, project_session_field, wait_terminal, InstanceProc,
    ServerProc, TestEnv, WsClient,
};

fn audited_load_ids(env: &TestEnv) -> Result<Vec<String>, String> {
    let body = std::fs::read_to_string(&env.acp_audit_file)
        .map_err(|error| format!("读取 ACP wire 审计失败: {error}"))?;
    body.lines()
        .map(|line| {
            let value: serde_json::Value = serde_json::from_str(line)
                .map_err(|error| format!("ACP wire 审计 JSON 非法: {error}"))?;
            if value["method"] != "session/load" {
                return Err(format!("ACP wire 审计出现未知方法: {value}"));
            }
            value["sessionId"]
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| format!("ACP wire 审计缺 sessionId: {value}"))
        })
        .collect()
}

struct HttpResponse {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl HttpResponse {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

async fn auth_request(
    port: u16,
    method: &str,
    cookie: Option<&str>,
    body: Option<&str>,
) -> Result<HttpResponse, String> {
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", port))
        .await
        .map_err(|e| format!("auth HTTP connect 失败: {e}"))?;
    let body = body.unwrap_or_default();
    let mut request = format!(
        "{method} /api/auth/session HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(cookie) = cookie {
        request.push_str(&format!("Cookie: {cookie}\r\n"));
    }
    request.push_str("\r\n");
    request.push_str(body);
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|e| format!("auth HTTP write 失败: {e}"))?;
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .await
        .map_err(|e| format!("auth HTTP read 失败: {e}"))?;
    let response = String::from_utf8(response).map_err(|e| format!("auth HTTP 非 UTF-8: {e}"))?;
    let (head, body) = response
        .split_once("\r\n\r\n")
        .ok_or_else(|| "auth HTTP response 缺 header 边界".to_string())?;
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| "auth HTTP response 缺 status".to_string())?;
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.to_string(), value.trim().to_string()))
        .collect();
    Ok(HttpResponse {
        status,
        headers,
        body: body.to_string(),
    })
}

async fn login(env: &TestEnv) -> Result<String, String> {
    let body = serde_json::json!({"token": env.client_token}).to_string();
    let response = auth_request(env.port, "POST", None, Some(&body)).await?;
    if response.status != 200 {
        return Err(format!(
            "登录失败 status={} body={}",
            response.status, response.body
        ));
    }
    assert_eq!(response.header("cache-control"), Some("no-store"));
    assert_eq!(response.header("pragma"), Some("no-cache"));
    assert_eq!(response.header("x-content-type-options"), Some("nosniff"));
    assert!(
        !response.body.contains(&env.client_token),
        "响应不得反射 bearer token"
    );
    let set_cookie = response
        .header("set-cookie")
        .ok_or_else(|| "登录响应缺 Set-Cookie".to_string())?;
    assert!(set_cookie.starts_with("acp_hub_session="));
    assert!(set_cookie.contains("HttpOnly"));
    assert!(set_cookie.contains("SameSite=Strict"));
    assert!(set_cookie.contains("Path=/"));
    assert!(set_cookie.contains("Max-Age=28800"));
    Ok(set_cookie
        .split(';')
        .next()
        .expect("cookie pair")
        .to_string())
}

async fn wait_prompt_delivery(
    client: &mut WsClient,
    command_id: &str,
    chat_doc: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = std::time::Instant::now() + timeout;
    let mut saw_update = false;
    let mut saw_terminal = false;
    while std::time::Instant::now() < deadline && (!saw_update || !saw_terminal) {
        let remaining = deadline
            .checked_duration_since(std::time::Instant::now())
            .unwrap_or(Duration::from_millis(1));
        match client.recv(remaining).await? {
            Some(Frame::YsyncUpdate(update)) if update.doc.as_str() == chat_doc => {
                saw_update = true;
            }
            Some(Frame::ActionAck(ack))
                if ack.command_id == command_id
                    && matches!(ack.status, AckStatus::Committed | AckStatus::Duplicate) =>
            {
                saw_terminal = true;
            }
            Some(Frame::ActionError(error)) if error.command_id == command_id => {
                return Err(format!(
                    "restored chat/prompt 失败: {:?} {}",
                    error.code, error.message
                ));
            }
            Some(_) => {}
            None => return Err("恢复 prompt 等待期间 WS 关闭".to_string()),
        }
    }
    if !saw_terminal || !saw_update {
        return Err(format!(
            "恢复 prompt 证据不完整: committed={saw_terminal} chat_update={saw_update}"
        ));
    }
    Ok(())
}

fn committed(frame: Frame, operation: &str) -> Result<acp_hub_proto::ack::ActionAck, String> {
    match frame {
        Frame::ActionAck(ack)
            if matches!(ack.status, AckStatus::Committed | AckStatus::Duplicate) =>
        {
            Ok(ack)
        }
        Frame::ActionAck(ack) => Err(format!("{operation} 意外 ack: {:?}", ack.status)),
        Frame::ActionError(error) => Err(format!(
            "{operation} 失败: {:?} {}",
            error.code, error.message
        )),
        _ => unreachable!("wait_terminal only returns terminal frames"),
    }
}

#[tokio::test]
async fn web_project_session_survives_restart_and_rebinds_exact_acp_id() -> Result<(), String> {
    println!("T-web-project-session-restart: START");
    let env = TestEnv::new();
    let mut server = ServerProc::start(&env, None);
    server.wait_ready()?;
    let mut instance = InstanceProc::start(&env);
    if !instance.wait_authenticated(Duration::from_secs(15)) {
        return Err("instance 初次认证超时".to_string());
    }

    let cookie = login(&env).await?;
    let status = auth_request(env.port, "GET", Some(&cookie), None).await?;
    assert_eq!(status.status, 200);
    assert!(status.body.contains("\"role\":\"full\""));
    let (mut client, _snapshot) =
        WsClient::connect_cookie(env.port, &cookie, &["hub:registry"]).await?;

    let project_command = uuid::Uuid::new_v4().to_string();
    client
        .send(&Frame::Action(ActionEnvelope::ProjectCreate {
            command_id: project_command,
            payload: ProjectCreatePayload {
                name: "E2E Project".to_string(),
                cwd: env.tmp.path().display().to_string(),
                instance_id: None,
            },
        }))
        .await?;
    let project_ack = committed(
        wait_terminal(&mut client, Duration::from_secs(20)).await?,
        "project/create",
    )?;
    let project_id = project_ack
        .project_id
        .ok_or_else(|| "project/create committed 缺 projectId".to_string())?;

    let session_command = uuid::Uuid::new_v4().to_string();
    client
        .send(&Frame::Action(ActionEnvelope::PersistedSessionCreate {
            command_id: session_command,
            payload: PersistedSessionCreatePayload {
                project_id: project_id.clone(),
                title: Some("Restart contract".to_string()),
            },
        }))
        .await?;
    let session_ack = committed(
        wait_terminal(&mut client, Duration::from_secs(35)).await?,
        "session/create",
    )?;
    let logical_session_id = session_ack
        .session_id
        .ok_or_else(|| "session/create committed 缺 sessionId".to_string())?;
    let first_chat_id = session_ack
        .chat_id
        .ok_or_else(|| "session/create committed 缺 chatId".to_string())?;
    let acp_session_id = session_ack
        .acp_session_id
        .ok_or_else(|| "session/create committed 缺 acpSessionId".to_string())?;

    let prompt_command = uuid::Uuid::new_v4().to_string();
    client
        .send(&Frame::Action(ActionEnvelope::Prompt {
            command_id: prompt_command,
            payload: PromptChatPayload {
                chat_id: first_chat_id.clone(),
                message: "persist me".to_string(),
                effort: None,
            },
        }))
        .await?;
    committed(
        wait_terminal(&mut client, Duration::from_secs(20)).await?,
        "first chat/prompt",
    )?;
    let _ = client.ws.close(None).await;

    instance.kill();
    server.kill();

    server = ServerProc::start(&env, None);
    server.wait_ready()?;
    let old_cookie = auth_request(env.port, "GET", Some(&cookie), None).await?;
    assert_eq!(
        old_cookie.status, 401,
        "browser session 必须是进程内生命周期"
    );
    instance = InstanceProc::start(&env);
    if !instance.wait_authenticated(Duration::from_secs(15)) {
        return Err("instance 重启认证超时".to_string());
    }

    let cookie = login(&env).await?;
    let (mut restored_client, snapshots) =
        WsClient::connect_cookie(env.port, &cookie, &["hub:registry"]).await?;
    let registry = doc_from_snapshots(&snapshots, "hub:registry")?;
    assert_eq!(
        project_field(&registry, &project_id, "name").as_deref(),
        Some("E2E Project")
    );
    assert_eq!(
        project_session_field(&registry, &logical_session_id, "acp_session_id").as_deref(),
        Some(acp_session_id.as_str())
    );
    assert_eq!(
        project_session_field(&registry, &logical_session_id, "lifecycle").as_deref(),
        Some("ready")
    );
    assert_eq!(
        project_session_field(&registry, &logical_session_id, "active_chat_id"),
        None,
        "server 重启后 runtime chat hint 不得冒充已恢复 runtime"
    );

    let open_command = uuid::Uuid::new_v4().to_string();
    restored_client
        .send(&Frame::Action(ActionEnvelope::PersistedSessionOpen {
            command_id: open_command,
            payload: PersistedSessionOpenPayload {
                session_id: logical_session_id.clone(),
            },
        }))
        .await?;
    let open_ack = committed(
        wait_terminal(&mut restored_client, Duration::from_secs(35)).await?,
        "session/open",
    )?;
    assert_eq!(
        open_ack.session_id.as_deref(),
        Some(logical_session_id.as_str())
    );
    assert_eq!(
        open_ack.acp_session_id.as_deref(),
        Some(acp_session_id.as_str())
    );
    let restored_chat_id = open_ack
        .chat_id
        .ok_or_else(|| "session/open committed 缺 chatId".to_string())?;
    assert_ne!(
        restored_chat_id, first_chat_id,
        "重启后必须创建新的 runtime chat"
    );
    assert_eq!(
        audited_load_ids(&env)?,
        vec![acp_session_id.clone()],
        "新的 ACP 进程必须在 stdin wire 上收到 SQLite 恢复出的精确 durable session id"
    );

    let restored_chat_doc = format!("chat:{restored_chat_id}");
    restored_client
        .send(&Frame::YsyncSubscribe(
            acp_hub_proto::ysync::YsyncSubscribe {
                docs: vec![restored_chat_doc.parse().expect("valid chat doc id")],
            },
        ))
        .await?;
    restored_client
        .recv_until(
            |frame| {
                matches!(frame, Frame::YsyncUpdate(update) if update.doc.as_str() == restored_chat_doc)
            },
            Duration::from_secs(10),
        )
        .await?;

    let restored_prompt = uuid::Uuid::new_v4().to_string();
    restored_client
        .send(&Frame::Action(ActionEnvelope::Prompt {
            command_id: restored_prompt.clone(),
            payload: PromptChatPayload {
                chat_id: restored_chat_id,
                message: "after exact load".to_string(),
                effort: None,
            },
        }))
        .await?;
    wait_prompt_delivery(
        &mut restored_client,
        &restored_prompt,
        &restored_chat_doc,
        Duration::from_secs(20),
    )
    .await?;

    let logout = auth_request(env.port, "DELETE", Some(&cookie), None).await?;
    assert_eq!(logout.status, 204);
    assert!(logout
        .header("set-cookie")
        .is_some_and(|value| value.contains("Max-Age=0")));
    let logged_out = auth_request(env.port, "GET", Some(&cookie), None).await?;
    assert_eq!(logged_out.status, 401);

    println!(
        "T-web-project-session-restart: PASS project={project_id} logical_session={logical_session_id} acp_session={acp_session_id}"
    );
    Ok(())
}
