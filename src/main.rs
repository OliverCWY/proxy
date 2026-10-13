use std::{path::Path, sync::Arc};

use proxy::{
    proxies::{
        inbound::InboundType,
        outbound::{OutboundProxyDyn, SimpleProxy},
    },
    utils::parse_proxy,
};
use tokio::{
    fs::File,
    io::{AsyncBufReadExt, BufReader, copy_bidirectional},
    net::TcpListener,
};

async fn parse_proxy_list(path: &Path) -> anyhow::Result<Vec<SimpleProxy>> {
    let reader = BufReader::new(File::open(path).await?);
    let mut lines = reader.lines();
    let mut proxies = vec![];
    while let Some(line) = lines.next_line().await? {
        if let Some(proxy) = parse_proxy(&line) {
            proxies.push(proxy);
        }
    }

    Ok(proxies)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let port = std::env::var("proxy_port")
        .unwrap_or("1099".to_string())
        .parse::<u16>()?;
    let listener = TcpListener::bind(("0.0.0.0", port)).await?;
    let the_proxy = match std::env::var("proxy_type")
        .unwrap_or("no-proxy".to_string())
        .as_str()
    {
        "no-proxy" => SimpleProxy::NoProxy,
        "http" => parse_proxy("http://lbhts51pgjj7:tq2e5w2wrqx3rz3@104.207.53.122:3129")
            .expect("format wrong!"),
        "socks5" => parse_proxy("socks5h://lbhts51pgjj7:tq2e5w2wrqx3rz3@104.207.53.122:1081")
            .expect("format wrong!"),
        _ => unreachable!(),
    };
    dbg!(&the_proxy);

    loop {
        let (socket, addr) = listener.accept().await?;
        println!("\n[+] New connection from {}", addr);

        let proxy = the_proxy.clone();
        tokio::spawn(async move {
            match InboundType::new(socket).await {
                Ok(InboundType::Forward(fwd)) => {
                    let (authed, auth) = fwd.handshake_must_auth().await?;
                    if auth.user != "user" || auth.pass != "pass" {
                        authed.reject().await?;
                        return Ok(());
                    }
                    let (ready, target) = authed.get_target().await?;
                    match proxy.connect(&target).await {
                        Ok(mut proxy_stream) => {
                            let mut client = ready.success().await?;
                            copy_bidirectional(&mut proxy_stream, &mut client)
                                .await
                                .map(|_| ())
                                .map_err(|e| anyhow::Error::from(e))
                        }
                        Err(e) => ready.fail(e).await,
                    }
                }
                Ok(InboundType::Reverse(_stream)) => {
                    println!("=> Matched Reverse (Normal HTTP or other)");
                    Ok(())
                }
                Err(e) => {
                    eprintln!("=> Error parsing request: {}", e);
                    Ok(())
                }
            }
        });
    }
}
