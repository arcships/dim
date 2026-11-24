use dim_agent_capabilities::{adapters::MockTools, defaults};
use dim_agent_runtime::{AgentBuilder, AgentConfig};
use dim_app_server::{ServerConfig, ShutdownReason, serve};
use serde_json::{Value, json};
use std::{
    mem::size_of,
    os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle},
    ptr::{addr_of, null_mut},
    sync::Arc,
    time::Duration,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader},
    net::windows::named_pipe::{ClientOptions, NamedPipeClient, ServerOptions},
};
use windows_sys::Win32::{
    Foundation::LocalFree,
    Security::{
        ACCESS_ALLOWED_ACE,
        Authorization::{GetSecurityInfo, SE_KERNEL_OBJECT},
        DACL_SECURITY_INFORMATION, EqualSid, GetAce, GetSecurityDescriptorControl,
        GetTokenInformation, OWNER_SECURITY_INFORMATION, SE_DACL_PROTECTED, TOKEN_QUERY,
        TOKEN_USER, TokenUser,
    },
    System::{
        SystemServices::ACCESS_ALLOWED_ACE_TYPE,
        Threading::{GetCurrentProcess, OpenProcessToken},
    },
};

fn builder() -> AgentBuilder {
    let mut builder = AgentBuilder::new(AgentConfig::default());
    defaults::install(&mut builder);
    defaults::mock_model_hosts(&mut builder);
    defaults::tool_host(&mut builder, Arc::new(MockTools));
    builder
}

async fn request(
    client: &mut BufReader<NamedPipeClient>,
    id: u32,
    method: &str,
    params: Value,
) -> Value {
    let mut frame = serde_json::to_vec(&json!({
        "jsonrpc":"2.0", "id":id, "method":method, "params":params,
    }))
    .unwrap();
    frame.push(b'\n');
    client.get_mut().write_all(&frame).await.unwrap();
    loop {
        let mut line = String::new();
        assert_ne!(client.read_line(&mut line).await.unwrap(), 0);
        let reply: Value = serde_json::from_str(&line).unwrap();
        if reply["id"] == id {
            assert!(reply.get("error").is_none(), "{reply}");
            return reply["result"].clone();
        }
    }
}

fn assert_current_user_only(client: &NamedPipeClient) {
    unsafe {
        let mut owner = null_mut();
        let mut dacl = null_mut();
        let mut descriptor = null_mut();
        assert_eq!(
            GetSecurityInfo(
                client.as_raw_handle(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION | OWNER_SECURITY_INFORMATION,
                &mut owner,
                null_mut(),
                &mut dacl,
                null_mut(),
                &mut descriptor,
            ),
            0,
        );
        let mut control = 0;
        let mut revision = 0;
        assert_ne!(
            GetSecurityDescriptorControl(descriptor, &mut control, &mut revision),
            0
        );
        assert_ne!(control & SE_DACL_PROTECTED, 0);
        assert!(!dacl.is_null());
        assert_eq!((*dacl).AceCount, 1);
        let mut ace = null_mut();
        assert_ne!(GetAce(dacl, 0, &mut ace), 0);
        let ace = ace.cast::<ACCESS_ALLOWED_ACE>();
        assert_eq!((*ace).Header.AceType, ACCESS_ALLOWED_ACE_TYPE as u8);

        let mut token = null_mut();
        assert_ne!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token),
            0
        );
        let _token = OwnedHandle::from_raw_handle(token);
        let mut length = 0;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut length);
        let mut user = vec![0usize; (length as usize).div_ceil(size_of::<usize>())];
        assert_ne!(
            GetTokenInformation(
                token,
                TokenUser,
                user.as_mut_ptr().cast(),
                length,
                &mut length,
            ),
            0
        );
        assert_ne!(
            EqualSid(
                addr_of!((*ace).SidStart).cast_mut().cast(),
                (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid,
            ),
            0
        );
        assert_ne!(
            EqualSid(owner, (*user.as_ptr().cast::<TOKEN_USER>()).User.Sid),
            0
        );
        LocalFree(descriptor);
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn native_client_authenticates_before_rpc_and_exits_without_stdin_eof() {
    use std::process::Stdio;
    tokio::time::timeout(Duration::from_secs(15), async {
        let name = format!(r"\\.\pipe\dim-native-client-{}", uuid::Uuid::new_v4());
        let server = serve(builder(), ServerConfig {
            pipe_name: Some(name.clone()), websocket_address: None, ..Default::default()
        }).await.unwrap();
        let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_dim-app-server"))
            .args(["--connect-pipe", &name])
            .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped())
            .kill_on_drop(true).spawn().unwrap();
        let mut status = BufReader::new(child.stderr.take().unwrap());
        let mut line = String::new();
        status.read_line(&mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap(), json!({"status":"ready"}));
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        stdin.write_all(b"{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"hello\",\"params\":{\"protocolVersion\":3,\"clientKind\":\"cli\"}}\n").await.unwrap();
        line.clear();
        stdout.read_line(&mut line).await.unwrap();
        assert_eq!(serde_json::from_str::<Value>(&line).unwrap()["result"]["protocolVersion"], 3);
        server.restart("Transport").await.unwrap();
        child.wait().await.unwrap();
        drop(stdin);
        let mut observer = BufReader::new(ClientOptions::new().open(&name).unwrap());
        request(&mut observer, 2, "hello", json!({"protocolVersion":3,"clientKind":"cli"})).await;
        request(&mut observer, 3, "daemon.shutdown", json!({})).await;
        assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
    }).await.unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn named_pipe_is_private_exclusive_and_shares_session_cleanup() {
    tokio::time::timeout(Duration::from_secs(15), async {
        let name = format!(r"\\.\pipe\dim-agent-test-{}", uuid::Uuid::new_v4());
        let config = ServerConfig {
            pipe_name: Some(name.clone()),
            websocket_address: None,
            ..Default::default()
        };
        let server = serve(builder(), config.clone()).await.unwrap();
        assert_eq!(server.pipe_name.as_deref(), Some(name.as_str()));
        assert!(serve(builder(), config).await.is_err());
        let mut client = BufReader::new(ClientOptions::new().open(&name).unwrap());
        assert_current_user_only(client.get_ref());
        let hello = request(
            &mut client,
            1,
            "hello",
            json!({"protocolVersion":3,"clientKind":"cli"}),
        )
        .await;
        assert_eq!(hello["protocolVersion"], 3);
        let created = request(
            &mut client,
            2,
            "session.create",
            json!({
                "cwd": std::env::temp_dir(), "tools":["echo"],
                "permissionPolicy":{"allowedTools":["echo"]},
            }),
        )
        .await;
        let sid = created["sessionId"].as_str().unwrap();
        request(&mut client, 3, "session.attach", json!({"sessionId":sid})).await;
        request(
            &mut client,
            4,
            "session.submit",
            json!({"sessionId":sid,"branchId":"main","text":"hello"}),
        )
        .await;
        request(
            &mut client,
            5,
            "session.idle",
            json!({"sessionId":sid,"branchId":"main"}),
        )
        .await;
        request(
            &mut client,
            6,
            "session.stop",
            json!({"sessionId":sid,"branchId":"main"}),
        )
        .await;
        request(&mut client, 7, "session.detach", json!({"sessionId":sid})).await;
        server.restart("Transport").await.unwrap();
        client.read_to_end(&mut Vec::new()).await.unwrap();
        drop(client);
        let mut client = BufReader::new(ClientOptions::new().open(&name).unwrap());
        request(
            &mut client,
            8,
            "hello",
            json!({"protocolVersion":3,"clientKind":"cli"}),
        )
        .await;
        request(&mut client, 11, "session.attach", json!({"sessionId":sid})).await;
        request(&mut client, 9, "session.close", json!({"sessionId":sid})).await;
        request(&mut client, 10, "daemon.shutdown", json!({})).await;
        assert_eq!(server.join().await.unwrap(), ShutdownReason::Graceful);
        client.read_to_end(&mut Vec::new()).await.unwrap();
        drop(client);
        assert!(ClientOptions::new().open(&name).is_err());
        let listener = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)
            .unwrap();
        drop(listener);
    })
    .await
    .unwrap();
}
