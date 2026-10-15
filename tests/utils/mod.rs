use fastrand::Rng;
use proxy::proxies::inbound::InboundAuth;

// Sorry but I have to use https...
pub const TEST_UPSTREAM_URL: &str = "https://captive.apple.com/hotspot-detect.html";
pub const RANDOM_PORT: &str = "127.0.0.1:0";

pub fn get_proxy(proto: &str, auth: Option<InboundAuth>, port: u16) -> String {
    if let Some(auth) = auth {
        format!("{}://{}:{}@127.0.0.1:{}", proto, auth.user, auth.pass, port)
    } else {
        format!("{}://127.0.0.1:{}", proto, port)
    }
}

pub async fn timeout<T>(secs: u64, task: impl Future<Output = T>) -> T {
    match tokio::time::timeout(std::time::Duration::from_secs(secs), task).await {
        Ok(res) => res,
        Err(_) => {
            eprintln!("\n================ STUCK FUTURES DUMP ================");
            eprintln!("{}", async_backtrace::taskdump_tree(false));
            eprintln!("====================================================\n");
            panic!("Test timed out, check dump above!");
        }
    }
}

// 固定一个全局基础 seed
pub const BASE_SEED: u64 = 0x2024_cafe_babe;

/// 保证生成的字符是 URL 安全的 (a-z, A-Z, 0-9)
pub fn random_alphanumeric(rng: &mut Rng, len: usize) -> String {
    std::iter::repeat_with(|| rng.alphanumeric())
        .take(len)
        .collect()
}

#[derive(Debug, Clone, Copy)]
pub enum AuthScenario {
    NoAuth,
    BothEmpty,      // user="", pass=""
    EmptyUser(u64), // user="", pass=随机(带seed)
    EmptyPass(u64), // user=随机(带seed), pass=""
    Random(u64),    // user与pass均随机(带seed)
}

impl AuthScenario {
    pub fn into_auth(self) -> Option<InboundAuth> {
        match self {
            AuthScenario::NoAuth => None,
            AuthScenario::BothEmpty => Some(InboundAuth {
                user: String::new(),
                pass: String::new(),
            }),
            AuthScenario::EmptyUser(seed) => {
                let mut rng = Rng::with_seed(BASE_SEED ^ seed);
                Some(InboundAuth {
                    user: String::new(),
                    pass: random_alphanumeric(&mut rng, 12),
                })
            }
            AuthScenario::EmptyPass(seed) => {
                let mut rng = Rng::with_seed(BASE_SEED ^ seed);
                Some(InboundAuth {
                    user: random_alphanumeric(&mut rng, 8),
                    pass: String::new(),
                })
            }
            AuthScenario::Random(seed) => {
                let mut rng = Rng::with_seed(BASE_SEED ^ seed);
                let u_len = rng.usize(4..12);
                let p_len = rng.usize(6..16);
                Some(InboundAuth {
                    user: random_alphanumeric(&mut rng, u_len),
                    pass: random_alphanumeric(&mut rng, p_len),
                })
            }
        }
    }
    pub fn inbound_fail_message(&self, proto: &str) -> Option<&'static str> {
        if matches!(self, AuthScenario::BothEmpty) {
            match proto {
                "socks5" | "socks5h" => Some("No supported method is provided"),
                "http" => Some("No auth provided"),
                _ => unreachable!(),
            }
        } else {
            None
        }
    }
    pub fn outbound_fail_message(&self, proto: &str) -> Option<&'static str> {
        if proto == "http" {
            return None;
        }
        match self {
            Self::EmptyPass(_) => Some("Empty password received"),
            Self::EmptyUser(_) | Self::BothEmpty => Some("Empty username received"),
            _ => None,
        }
    }
}
