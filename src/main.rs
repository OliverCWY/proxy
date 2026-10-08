use std::{net::IpAddr, path::Path, sync::Arc};

use proxy::{
    pool::ProxyPool,
    proxies::{
        Endpoint, Host,
        inbound::{Inbound, parse_inbound_request},
        outbound::{OutboundProxyDyn, SimpleProxy},
    },
    utils::is_valid_proxy,
};
use smallvec::{SmallVec, smallvec};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader, copy_bidirectional},
    net::TcpListener,
};

async fn parse_proxy_list(path: &Path) -> anyhow::Result<Vec<Arc<str>>> {
    let reader = BufReader::new(File::open(path).await?);
    let mut lines = reader.lines();
    let mut proxies = vec![];
    while let Some(line) = lines.next_line().await? {
        if !is_valid_proxy(&line) {
            continue;
        }

        proxies.push(Arc::from(line));
    }

    Ok(proxies)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // let proxies = parse_proxy_list(Path::new("target/proxies_gn_001722.txt")).await?;
    // let pool = ProxyPool::new(128, proxies);
    let port = std::env::var("proxy_port")
        .unwrap_or("1099".to_string())
        .parse::<u16>()?;
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    let user = b"lbhts51pgjj7";
    let pass = b"tq2e5w2wrqx3rz3";
    let proxy_host = Host::Ip("104.207.53.122".parse::<IpAddr>()?);
    let the_proxy = Arc::new(
        match std::env::var("proxy_type")
            .unwrap_or("no-proxy".to_string())
            .as_str()
        {
            "no-proxy" => SimpleProxy::NoProxy,
            "http" => {
                let mut auth = Vec::new();
                auth.extend_from_slice(user);
                auth.push(b':');
                auth.extend_from_slice(pass);
                use base64::prelude::*;
                let auth = BASE64_STANDARD.encode(auth);
                let mut auth_header = Vec::from(b"Proxy-Authorization: Basic ");
                auth_header.extend(auth.into_bytes());
                auth_header.extend(b"\r\n");
                SimpleProxy::Http {
                    auth_header: SmallVec::from_slice(&auth_header),
                    host: Endpoint {
                        host: proxy_host,
                        port: 3129,
                    },
                }
            }
            "socks5" => {
                let mut auth: proxy::proxies::outbound::Auth = smallvec![0x01];
                auth.push(user.len() as u8);
                auth.extend_from_slice(user);
                auth.push(pass.len() as u8);
                auth.extend_from_slice(pass);

                SimpleProxy::Socks5 {
                    auth,
                    host: Endpoint {
                        host: proxy_host,
                        port: 1081,
                    },
                    dns: false,
                }
            }
            _ => unreachable!(),
        },
    );
    loop {
        let (socket, addr) = listener.accept().await?;
        println!("\n[+] New connection from {}", addr);

        let proxy = the_proxy.clone();
        tokio::spawn(async move {
            match parse_inbound_request(socket).await {
                Ok(Inbound::Forward(target, auth, mut reader)) => {
                    println!(
                        "=> Matched HTTP CONNECT! Target: {:?}:{}",
                        target.host, target.port
                    );
                    if let Some(auth) = auth {
                        println!("=> Auth: {}:{}", auth.user, auth.pass);
                    }
                    let mut proxy = proxy.connect(&target).await?;
                    copy_bidirectional(&mut proxy, &mut reader).await?;
                }
                Ok(Inbound::Reverse(_stream)) => {
                    println!("=> Matched Reverse (Normal HTTP or other)");
                }
                Err((e, _)) => {
                    eprintln!("=> Error parsing request: {}", e);
                }
            }
            Ok::<(), anyhow::Error>(())
        });
    }
}
