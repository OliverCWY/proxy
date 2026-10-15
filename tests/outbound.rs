use std::{net::IpAddr, time::Duration};

use async_backtrace::framed;
use fast_socks5::{
    Socks5Command, client,
    server::{Socks5ServerProtocol, run_tcp_proxy},
    util::target_addr::TargetAddr,
};
use proxy::{
    proxies::{
        Endpoint,
        Host::{Domain, Ipv4, Ipv6},
        inbound::{InboundAuth, http::HttpInitState},
        outbound::{OutboundProxyDyn, SimpleProxy::NoProxy},
    },
    utils::parse_proxy,
};

mod utils;
use rstest::rstest;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt, copy_bidirectional},
    join,
    net::{TcpListener, TcpStream},
};
use utils::*;

#[framed]
async fn proxy_client(proxy: &str, target: Endpoint) -> anyhow::Result<()> {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let proxy = parse_proxy(proxy).unwrap();
    let mut stream = proxy.connect(&target).await?;

    let request = format!(
        "GET / HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
        str::from_utf8(target.get_str_bytes()?.as_ref())?
    );
    stream.write_all(request.as_bytes()).await?;

    let mut response = String::new();
    stream.read_to_string(&mut response).await?;

    dbg!(response);

    Ok(())
}

async fn socks5_server(
    stream: TcpStream,
    auth: Option<InboundAuth>,
    target: Endpoint,
    use_server_proxy: bool,
) -> anyhow::Result<()> {
    let protol = if let Some(auth) = auth {
        let (protol, successful) =
            Socks5ServerProtocol::accept_password_auth(stream, |user, pass| {
                dbg!(&user, &pass);
                user == auth.user && pass == auth.pass
            })
            .await?;
        assert!(successful);
        protol
    } else {
        Socks5ServerProtocol::accept_no_auth(stream).await?
    };
    let (proto, cmd, target_addr) = protol.read_command().await?;
    assert_eq!(cmd, Socks5Command::TCPConnect);
    let check_target = async || -> anyhow::Result<()> {
        match &target_addr {
            TargetAddr::Domain(domain, port) => {
                if use_server_proxy {
                    assert_eq!(
                        Endpoint {
                            host: proxy::proxies::Host::Domain(domain.as_str().into()),
                            port: *port
                        },
                        target
                    )
                } else {
                    panic!("Expected ip but got domain!")
                }
            }
            TargetAddr::Ip(socket_addr) => {
                assert_eq!(socket_addr.port(), target.port);
                match &target.host {
                    Ipv4(ip) => assert_eq!(socket_addr.ip(), IpAddr::V4(*ip)),
                    Ipv6(ip) => assert_eq!(socket_addr.ip(), IpAddr::V6(*ip)),
                    Domain(_) => {
                        let sockets = dbg!(target.get_socket_addr().await?);
                        assert!(sockets.contains(dbg!(socket_addr)));
                    }
                }
            }
        };
        Ok(())
    };
    check_target().await?;

    run_tcp_proxy(
        proto,
        &target_addr.resolve_dns().await?,
        Duration::from_secs(10),
        false,
    )
    .await?;
    Ok(())
}

async fn http_server(
    stream: TcpStream,
    auth: Option<InboundAuth>,
    target: Endpoint,
) -> anyhow::Result<()> {
    let state = HttpInitState::new(stream);
    let (state, client_auth) = state.handshake().await?;
    assert_eq!(auth, client_auth);
    let (state, client_target) = state.get_target();
    assert_eq!(target, client_target);
    let mut stream = state.success().await?;
    let mut upstream = NoProxy.connect(&target).await?;
    copy_bidirectional(&mut stream, &mut upstream).await?;

    Ok(())
}

async fn proxy_server(
    listener: TcpListener,
    auth: Option<InboundAuth>,
    proto: &str,
    target: Endpoint,
) -> anyhow::Result<()> {
    let stream = listener.accept().await?.0;

    match proto {
        "http" => http_server(stream, auth, target).await,
        "socks5" => socks5_server(stream, auth, target, false).await,
        "socks5h" => socks5_server(stream, auth, target, true).await,
        _ => unreachable!(),
    }
}

#[rstest]
#[tokio::test]
async fn test_outbound_proxy(
    #[values("http", "socks5", "socks5h")] proto: &'static str,
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
    #[values(true, false)] use_server_dns: bool,
) {
    let auth = scenario.into_auth();
    println!("Running {proto} test with: {auth:?}");

    let listener = TcpListener::bind(RANDOM_PORT).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let target = Endpoint {
        host: Domain("example.com".into()),
        port: 80,
    };
    let target = if !use_server_dns {
        let addrs = target.get_socket_addr().await.unwrap();
        let addr = addrs.into_iter().next().unwrap();
        addr.into()
    } else {
        target
    };

    let server = proxy_server(listener, auth.clone(), proto, target.clone());
    let proxy = get_proxy(proto, auth, port);
    let client = timeout(10, proxy_client(&proxy, target));
    let (server_res, client_res) = dbg!(join!(server, client));
    if let Some(msg) = scenario.outbound_fail_message(proto) {
        let error_msgs: Vec<String> = [server_res, client_res]
            .into_iter()
            .map(|res| res.expect_err(msg).to_string())
            .collect();
        if !error_msgs.iter().any(|err| err.contains(msg)) {
            panic!("Unexpected error messages: {:?}", error_msgs);
        }
    } else {
        server_res.expect("Server successfully ended.");
        client_res.expect("Client successfully ended.");
    }
}
