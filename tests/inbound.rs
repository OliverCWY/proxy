use async_backtrace::framed;
use proxy::proxies::{
    inbound::{InboundAuth, InboundType},
    outbound::{OutboundProxyDyn, SimpleProxy},
};
use rstest::rstest;
use tokio::{io::copy_bidirectional, join, net::TcpListener};

mod utils;
use utils::*;

#[framed]
async fn proxy_server(listener: TcpListener, auth: Option<InboundAuth>) -> anyhow::Result<()> {
    let (socket, addr) = listener.accept().await?;
    dbg!(addr);
    if let InboundType::Forward(fwd) = InboundType::new(socket).await? {
        let next = if let Some(auth) = auth {
            let (next, inbound_auth) = fwd.handshake_must_auth().await?;
            dbg!(&inbound_auth);
            assert_eq!(inbound_auth, auth);
            next
        } else {
            fwd.handshake_no_auth().await?
        };
        let (next, target) = next.get_target().await?;
        dbg!(&target);
        let proxy = SimpleProxy::NoProxy;
        let mut proxy = proxy.connect_inner(&target).await?;
        let mut inbound = next.success().await?;
        copy_bidirectional(&mut inbound, &mut proxy).await?;
        Ok(())
    } else {
        panic!()
    }
}

async fn proxy_client(proxy: String) -> anyhow::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let proxy = reqwest::Proxy::all(proxy)?;
    let client = reqwest::Client::builder().proxy(proxy).build()?;
    client
        .get(TEST_UPSTREAM_URL)
        .header("Connection", "close")
        .send()
        .await
        .map_err(Into::into)
        .map(|_| ())
}

#[rstest]
#[tokio::test]
async fn test_inbound_proxy(
    #[values("http", "socks5", "socks5h")] proto: &str,
    #[values(
        // 边界情况
        AuthScenario::NoAuth,
        AuthScenario::BothEmpty,
        AuthScenario::EmptyUser(1),
        AuthScenario::EmptyPass(2),
        // 多个固定 seed 的随机测试用例
        AuthScenario::Random(101),
        AuthScenario::Random(102),
        AuthScenario::Random(103),
        AuthScenario::Random(104),
        AuthScenario::Random(105),
    )]
    scenario: AuthScenario,
) {
    let auth = scenario.into_auth();
    println!("Running {proto} test with: {auth:?}");

    let listener = TcpListener::bind(RANDOM_PORT).await.unwrap();
    let port = listener.local_addr().unwrap().port();

    let server = proxy_server(listener, auth.clone());
    let client = proxy_client(get_proxy(proto, auth, port));
    let server = timeout(5, server);
    let (server_res, client_res) = join!(server, client);
    if let Some(msg) = scenario.inbound_fail_message(proto) {
        // 1. 预期失败：验证 Server 必须返回 Err
        let err = server_res.expect_err("Expected server handshake to fail, but it succeeded");

        // 精确匹配报错信息，确保不是因为其他莫名其妙的原因挂掉
        assert!(
            err.to_string().contains(msg),
            "Unexpected error message: {err:?}"
        );
    } else {
        // 2. 正常场景：两边都必须成功
        server_res.expect("Server handshake failed unexpectedly");
        client_res.expect("Client request failed unexpectedly");
    }
}

// TODO: add auth failed tests
