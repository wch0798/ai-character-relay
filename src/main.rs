use aic_relay::config::{valid_domain, valid_email, Config, DEFAULT_CONFIG_PATH};
use aic_relay::gate::{random_string, Gate, TokenFile};
use std::path::PathBuf;
use std::process::ExitCode;

const PROXY_WARNING: &str = "이 서버 앞에 프록시(Cloudflare 주황 구름, nginx 등)나 Docker 포트 중계가 있어 접속자 주소가 모두 같게 보입니다. 이 상태에서는 암호를 계속 틀리는 주소를 막을 수 없고 HTTPS 인증서 발급도 실패할 수 있습니다. DNS 를 'DNS만'(회색 구름)으로 바꾸고, Docker 는 network_mode: host 로 실행하세요.";

const HELP: &str = "AI Character 중계 서버

사용법:
  aic-relay init --domain <도메인> --email <이메일> [--data-dir <폴더>]
                                설정 파일과 첫 접속 암호를 만듭니다 (다시 실행하면 도메인·이메일만 바꿉니다)
  aic-relay run                 중계 서버를 실행합니다 (보통 systemd 가 실행)
  aic-relay status              실행 중인 서버의 상태를 봅니다
  aic-relay token list          접속 암호 목록
  aic-relay token add <이름>    접속 암호 만들기 (서버를 끄지 않아도 바로 적용)
  aic-relay token show <이름>   접속 암호 보기
  aic-relay token remove <이름> 접속 암호 지우기
  aic-relay unblock <IP>        암호를 여러 번 틀려 막힌 주소를 풉니다
  aic-relay version

공통 옵션:
  -c, --config <파일>           설정 파일 (기본: /etc/aic-relay/config.toml, 환경변수 AIC_RELAY_CONFIG)";

struct Args {
    words: Vec<String>,
    config: PathBuf,
    domain: Option<String>,
    email: Option<String>,
    data_dir: Option<PathBuf>,
}

fn parse_args() -> Result<Args, String> {
    let mut args = Args {
        words: Vec::new(),
        config: std::env::var_os("AIC_RELAY_CONFIG").map(PathBuf::from).unwrap_or_else(|| PathBuf::from(DEFAULT_CONFIG_PATH)),
        domain: None,
        email: None,
        data_dir: None,
    };
    let mut iter = std::env::args().skip(1);
    while let Some(arg) = iter.next() {
        let mut value = |flag: &str| iter.next().ok_or_else(|| format!("{flag} 뒤에 값을 넣어 주세요."));
        match arg.as_str() {
            "-c" | "--config" => args.config = PathBuf::from(value(&arg)?),
            "--domain" => args.domain = Some(value(&arg)?),
            "--email" => args.email = Some(value(&arg)?),
            "--data-dir" => args.data_dir = Some(PathBuf::from(value(&arg)?)),
            "-h" | "--help" => args.words = vec!["help".into()],
            _ => args.words.push(arg),
        }
    }
    Ok(args)
}

fn init(args: &Args) -> Result<(), String> {
    let domain = args.domain.as_deref().ok_or("--domain 을 넣어 주세요. 예) --domain relay.mydomain.com")?.trim().to_ascii_lowercase();
    let email = args.email.as_deref().ok_or("--email 을 넣어 주세요.")?.trim().to_string();
    if !valid_domain(&domain) {
        return Err(format!("도메인 형식이 아닙니다: {domain}"));
    }
    if !valid_email(&email) {
        return Err(format!("이메일 형식이 아닙니다: {email}"));
    }
    let mut config = match Config::load(&args.config) {
        Ok(existing) => existing,
        Err(_) => Config::new(&domain, &email, random_string(24)?),
    };
    config.domain = domain;
    config.email = email;
    if let Some(dir) = &args.data_dir {
        config.data_dir = dir.clone();
    }
    config.validate()?;
    if let Some(parent) = args.config.parent() {
        std::fs::create_dir_all(parent).map_err(|error| format!("{} 폴더를 만들지 못했습니다: {error}", parent.display()))?;
    }
    std::fs::write(&args.config, config.to_toml()).map_err(|error| format!("{} 에 저장하지 못했습니다: {error}", args.config.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&args.config, std::fs::Permissions::from_mode(0o640));
    }
    std::fs::create_dir_all(config.cert_dir()).map_err(|error| format!("{} 폴더를 만들지 못했습니다: {error}", config.data_dir.display()))?;
    let path = config.tokens_path();
    let mut tokens = TokenFile::load(&path)?;
    if tokens.tokens.is_empty() {
        tokens.add("기본")?;
        tokens.save(&path)?;
    }
    let first = &tokens.tokens[0];
    println!("설정 파일: {}", args.config.display());
    println!("중계 서버 주소 : {}", config.domain);
    println!("접속 암호      : {}  (이름: {})", first.token, first.name);
    println!("관리자 암호    : {}", config.admin_password);
    Ok(())
}

fn token_command(args: &Args) -> Result<(), String> {
    let config = Config::load(&args.config)?;
    let path = config.tokens_path();
    let mut file = TokenFile::load(&path)?;
    let name = args.words.get(2).map(String::as_str);
    match (args.words.get(1).map(String::as_str), name) {
        (Some("list") | None, _) => {
            if file.tokens.is_empty() {
                println!("접속 암호가 없습니다. `aic-relay token add 우리집` 으로 만드세요.");
            }
            for token in &file.tokens {
                println!("{}\t••••{}", token.name, &token.token[token.token.len().saturating_sub(4)..]);
            }
        }
        (Some("add"), Some(name)) => {
            let token = file.add(name)?;
            file.save(&path)?;
            println!("'{}' 접속 암호: {}", token.name, token.token);
        }
        (Some("show"), Some(name)) => {
            let token = file.tokens.iter().find(|token| token.name == name).ok_or(format!("'{name}' 이름의 암호가 없습니다."))?;
            println!("{}", token.token);
        }
        (Some("remove"), Some(name)) => {
            if !file.remove(name) {
                return Err(format!("'{name}' 이름의 암호가 없습니다."));
            }
            file.save(&path)?;
            println!("'{name}' 접속 암호를 지웠습니다. 새 연결부터 막힙니다.");
        }
        _ => return Err("사용법: aic-relay token list | add <이름> | show <이름> | remove <이름>".into()),
    }
    Ok(())
}

/// Calls the running server's local admin API.
async fn admin_call(config: &Config, method: &str, path: &str, body: Option<serde_json::Value>) -> Result<serde_json::Value, String> {
    use base64::Engine;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(config.admin_bind).await.map_err(|_| "서버가 실행 중이 아닙니다. `sudo systemctl status aic-relay` 로 확인하세요.".to_string())?;
    let auth = base64::engine::general_purpose::STANDARD.encode(format!("admin:{}", config.admin_password));
    let body = body.map(|value| value.to_string()).unwrap_or_default();
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAuthorization: Basic {auth}\r\nX-AIC-Admin: 1\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.map_err(|error| error.to_string())?;
    let mut response = String::new();
    stream.read_to_string(&mut response).await.map_err(|error| error.to_string())?;
    let body = response.split_once("\r\n\r\n").map(|(_, body)| body).unwrap_or_default();
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| "서버 응답을 읽지 못했습니다.".to_string())?;
    match value.get("error").and_then(serde_json::Value::as_str) {
        Some(error) => Err(error.to_string()),
        None => Ok(value),
    }
}

async fn unblock(args: &Args) -> Result<(), String> {
    let config = Config::load(&args.config)?;
    let ip = args.words.get(1).ok_or("사용법: aic-relay unblock <IP 주소>")?;
    admin_call(&config, "POST", "/api/unblock", Some(serde_json::json!({ "ip": ip }))).await?;
    println!("{ip} 를 풀었습니다.");
    Ok(())
}

async fn status(args: &Args) -> Result<(), String> {
    let config = Config::load(&args.config)?;
    let value = admin_call(&config, "GET", "/api/status", None).await?;
    let uptime = value["uptimeSecs"].as_u64().unwrap_or(0);
    println!("도메인         : {}", value["domain"].as_str().unwrap_or_default());
    println!("가동 시간      : {}시간 {}분", uptime / 3600, uptime % 3600 / 60);
    println!("연결된 기기    : {} (연결 {})", value["liveDevices"], value["liveConnections"]);
    println!("누적 허용/거절 : {} / {}", value["accepted"], value["denied"]);
    let day = &value["hub"]["paths"]["day"];
    let (direct, relayed) = (day["direct"].as_u64().unwrap_or(0), day["relayed"].as_u64().unwrap_or(0));
    if direct + relayed > 0 {
        println!("직접 연결 비율 : {}% (24시간, 직접 {direct} / 중계 {relayed})", direct * 100 / (direct + relayed));
    }
    let blocked = value["guard"]["blocked"].as_array().cloned().unwrap_or_default();
    println!("막힌 주소      : {}개", blocked.len());
    for entry in &blocked {
        println!("  - {} ({}분 남음)", entry["ip"].as_str().unwrap_or_default(), entry["remainingSecs"].as_u64().unwrap_or(0) / 60 + 1);
    }
    if value["guard"]["sharedAddress"] == true || value["guard"]["proxyHeaders"] == true {
        println!("⚠ {PROXY_WARNING}");
    }
    for token in value["tokens"].as_array().into_iter().flatten() {
        println!("  - {} : 연결 {}", token["name"].as_str().unwrap_or_default(), token["live"]);
    }
    Ok(())
}

async fn shutdown_signal() {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()).expect("signal handler");
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = term.recv() => {}
        }
    }
    #[cfg(not(unix))]
    let _ = tokio::signal::ctrl_c().await;
}

async fn run(args: &Args) -> Result<(), String> {
    let config = Config::load(&args.config)?;
    let gate = Gate::open(&config.tokens_path())?;
    if gate.tokens().is_empty() {
        eprintln!("경고: 접속 암호가 없어 아무도 연결할 수 없습니다. `aic-relay token add 우리집` 으로 만드세요.");
    }
    let mut server = aic_relay::spawn(&config, gate).await?;
    eprintln!(
        "AI Character 중계 서버 v{} 시작: {} ({}), 관리 화면 http://{}{}",
        env!("CARGO_PKG_VERSION"),
        config.domain,
        if config.dev { "개발 모드, http" } else { "https, 인증서 자동" },
        server.admin_addr,
        if config.https_admin && !config.dev { format!(" 와 https://{}/admin/", config.domain) } else { String::new() }
    );
    tokio::select! {
        _ = shutdown_signal() => {}
        result = server.join() => eprintln!("중계 서버가 멈췄습니다: {result:?}"),
    }
    server.shutdown().await.map_err(|error| error.to_string())
}

#[tokio::main]
async fn main() -> ExitCode {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let args = match parse_args() {
        Ok(args) => args,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let result = match args.words.first().map(String::as_str) {
        Some("run") => run(&args).await,
        Some("init") => init(&args),
        Some("token") => token_command(&args),
        Some("status") => status(&args).await,
        Some("unblock") => unblock(&args).await,
        Some("version") => {
            println!("aic-relay {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        None | Some("help") => {
            println!("{HELP}");
            Ok(())
        }
        Some(other) => Err(format!("모르는 명령입니다: {other}\n\n{HELP}")),
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}
