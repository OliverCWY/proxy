use std::{path::Path, sync::Arc};

use proxy::{pool::ProxyPool, proxies::inbound::{Inbound, parse_inbound_request}, utils::is_valid_proxy};
use tokio::{fs::File, io::{AsyncBufReadExt, BufReader}, net::TcpListener};

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
    let proxies = parse_proxy_list(Path::new("target/proxies_gn_001722.txt")).await?;
    let pool = ProxyPool::new(128, proxies);
    let listener = TcpListener::bind("0.0.0.0:8080").await?;
    loop {
        let (socket, addr) = listener.accept().await?;
        println!("\n[+] New connection from {}", addr);
        
        tokio::spawn(async move {
            match parse_inbound_request(socket).await {
                Ok(Inbound::Forward(target, auth, reader)) => {
                    println!("=> Matched HTTP CONNECT! Target: {}:{}", target.host, target.port);
                    if let Some(auth) = auth {println!("=> Auth: {}:{}", auth.user, auth.pass);}
                }
                Ok(Inbound::Reverse(_stream)) => {
                    println!("=> Matched Reverse (Normal HTTP or other)");
                }
                Err((e, _)) => {
                    eprintln!("=> Error parsing request: {}", e);
                }
            }
        });
    }
}
