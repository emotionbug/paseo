use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};
use tokio_util::sync::CancellationToken;

async fn fake_hub(responses: Vec<(u16, Value)>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let origin = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let mut requests = Vec::new();
        for (status, body) in responses {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let mut buffer = [0u8; 4096];
            loop {
                let count = socket.read(&mut buffer).await.unwrap();
                assert!(count > 0);
                bytes.extend_from_slice(&buffer[..count]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let header = String::from_utf8_lossy(&bytes[..end]);
                    let length = header
                        .lines()
                        .find_map(|line| {
                            line.split_once(':')
                                .filter(|(key, _)| key.eq_ignore_ascii_case("content-length"))
                                .map(|(_, length)| length.trim().parse::<usize>().unwrap())
                        })
                        .unwrap_or(0);
                    if bytes.len() >= end + 4 + length {
                        break;
                    }
                }
            }
            requests.push(String::from_utf8(bytes).unwrap());
            let body = serde_json::to_vec(&body).unwrap();
            socket.write_all(format!("HTTP/1.1 {status} Test\r\nContent-Type: {}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",if status>=400{"application/problem+json"}else{"application/json"},body.len()).as_bytes()).await.unwrap();
            socket.write_all(&body).await.unwrap();
            socket.shutdown().await.unwrap();
        }
        requests
    });
    (origin, task)
}
#[tokio::test]
async fn dry_run_validates_without_installing() {
    let root = tempfile::tempdir().unwrap();
    tokio::fs::create_dir_all(root.path().join(".paseo/triggers"))
        .await
        .unwrap();
    tokio::fs::write(root.path().join(".paseo/triggers/demo.yml"), "name: demo\n")
        .await
        .unwrap();
    let (origin, server) = fake_hub(vec![(200, json!({"name":"demo","valid":true}))]).await;
    let args = command()
        .try_get_matches_from(["hub", "deploy", "--dry-run"])
        .unwrap();
    let m = args.subcommand_matches("deploy").unwrap();
    let http = HubHttp::new(CancellationToken::new()).unwrap();
    let result = deploy(m, &http, &origin, "test-secret", root.path())
        .await
        .unwrap();
    assert_eq!(result.data[0]["valid"], true);
    let requests = server.await.unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0].starts_with("POST /api/v1/triggers/validate "));
    assert!(!requests[0].contains("/install"));
}
#[tokio::test]
async fn export_conflict_prevents_all_file_writes() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join(".paseo/triggers");
    tokio::fs::create_dir_all(&target).await.unwrap();
    tokio::fs::write(target.join("existing.yml"), "local")
        .await
        .unwrap();
    let (origin,server)=fake_hub(vec![(200,json!({"triggers":[{"id":uuid::Uuid::new_v4(),"name":"new","enabled":true,"format":"single_run","yaml":"new remote"},{"id":uuid::Uuid::new_v4(),"name":"existing","enabled":true,"format":"single_run","yaml":"remote"}]}))]).await;
    let args = command().try_get_matches_from(["hub", "export"]).unwrap();
    let m = args.subcommand_matches("export").unwrap();
    let http = HubHttp::new(CancellationToken::new()).unwrap();
    let error = export(m, &http, &origin, "test-secret", root.path())
        .await
        .err()
        .unwrap();
    assert_eq!(
        error
            .downcast_ref::<crate::client::ClientError>()
            .unwrap()
            .code,
        "HUB_EXPORT_CONFLICT"
    );
    assert!(!target.join("new.yml").exists());
    assert_eq!(
        tokio::fs::read_to_string(target.join("existing.yml"))
            .await
            .unwrap(),
        "local"
    );
    server.await.unwrap();
}
#[tokio::test]
async fn problem_details_redact_credentials_and_do_not_retry_mutations() {
    let secret = "private-test-credential";
    let (origin,server)=fake_hub(vec![(422,json!({"status":422,"title":format!("Rejected {secret}"),"errors":[{"path":[".paseo/triggers/a.yml","steps",0],"message":format!("Invalid {secret}")}]}))]).await;
    let http = HubHttp::new(CancellationToken::new()).unwrap();
    let error = http
        .post(
            &origin,
            "/api/v1/triggers/install",
            Some(secret),
            &json!({"yaml":"x"}),
            201,
        )
        .await
        .unwrap_err();
    assert!(!error.to_string().contains(secret));
    assert!(error.to_string().contains("[redacted]"));
    assert_eq!(server.await.unwrap().len(), 1);
}
