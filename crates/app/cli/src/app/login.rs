//! `sbx login`, `sbx logout`. 자격 증명은 sbx가 따로 두지 않는다: Docker의 `config.json`(또는 그 credential
//! helper, `REGISTRY_AUTH_FILE`)에 쓰고 지운다([`sbx_oci::auth`]). 그래서 docker, podman, skopeo와 함께 쓴다.
//!
//! 쓰기 전에 레지스트리에 확인한다(`GET /v2/`, Docker와 같다). 비밀번호는 `--password-stdin`이나, 표준 입력이
//! 터미널이면 화면에 보이지 않게 묻는다. 명령 줄 인자로는 받지 않는다(프로세스 목록과 셸 기록에 남는다).

use std::io::{BufRead, IsTerminal, Read, Write};

use sbx_oci::auth::{self, Credential, Stored};
use sbx_oci::registry::{LoginCheck, Registry};

use super::{AppResult, Ctx, msg};
use crate::output;

/// 사용자가 준 레지스트리(없으면 Docker Hub)를 호스트로.
fn host_of(server: Option<&str>) -> AppResult<String> {
    let host = auth::registry_host(server.unwrap_or(sbx_oci::reference::DOCKER_HUB));
    if host.is_empty() || host.starts_with('-') || host.contains(char::is_whitespace) {
        return Err(msg(format!(
            "invalid registry {:?}",
            server.unwrap_or_default()
        )));
    }
    Ok(host)
}

/// API 호스트. Docker Hub는 `registry-1.docker.io`다.
fn api_host(host: &str) -> &str {
    if host == sbx_oci::reference::DOCKER_HUB {
        "registry-1.docker.io"
    } else {
        host
    }
}

fn ask(prompt: &str) -> AppResult<String> {
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    std::io::stdin().lock().read_line(&mut line)?;
    Ok(line.trim_end_matches(['\r', '\n']).to_owned())
}

/// 화면에 보이지 않게 한 줄을 읽는다. Ctrl+C는 그만둔다.
fn ask_hidden(prompt: &str) -> AppResult<String> {
    use crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers, read};
    eprint!("{prompt}");
    let _ = std::io::stderr().flush();
    crossterm::terminal::enable_raw_mode()?;
    let mut s = String::new();
    let result = loop {
        match read() {
            Ok(Event::Key(k)) if k.kind != KeyEventKind::Release => match k.code {
                KeyCode::Enter => break Ok(()),
                KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => {
                    break Err(msg("cancelled"));
                }
                KeyCode::Char(c) => s.push(c),
                KeyCode::Backspace => {
                    s.pop();
                }
                _ => {}
            },
            Ok(_) => {}
            Err(e) => break Err(e.into()),
        }
    };
    let _ = crossterm::terminal::disable_raw_mode();
    eprintln!();
    result.map(|()| s)
}

impl Ctx {
    /// `sbx login [-u USER] [--password-stdin] [SERVER]`.
    pub(super) async fn login(
        &self,
        username: Option<&str>,
        password_stdin: bool,
        server: Option<&str>,
    ) -> AppResult<u8> {
        let host = host_of(server)?;
        let terminal = std::io::stdin().is_terminal();
        let username = match username {
            Some(u) => u.to_owned(),
            None if password_stdin || !terminal => {
                return Err(msg(
                    "give the user name with -u (it can't be asked for here)",
                ));
            }
            None => ask("Username: ")?,
        };
        if username.is_empty() || username.contains(':') && username != "<token>" {
            return Err(msg("invalid user name"));
        }
        let secret = if password_stdin {
            let mut s = String::new();
            std::io::stdin()
                .lock()
                .take(64 << 10)
                .read_to_string(&mut s)?;
            s.trim_end_matches(['\r', '\n']).to_owned()
        } else if terminal {
            ask_hidden("Password: ")?
        } else {
            return Err(msg("give the password with --password-stdin"));
        };
        if secret.is_empty() {
            return Err(msg("the password is empty"));
        }
        let cred = Credential { username, secret };
        // registries.conf의 `insecure`(http, 자체 서명 인증서)는 podman login처럼 따른다.
        let insecure = sbx_oci::registries::Registries::load(&crate::dirs::registries_conf())?
            .insecure_hosts();
        match Registry::new()?
            .with_insecure(insecure)?
            .check_login(api_host(&host), &cred)
            .await?
        {
            LoginCheck::Accepted => {}
            LoginCheck::NotAsked => {
                eprintln!("sbx: {host} doesn't ask for credentials; saving them anyway");
            }
        }
        let files = crate::dirs::registry_auth();
        let registry = host.clone();
        let stored = tokio::task::spawn_blocking(move || auth::store(&files, &host, &cred))
            .await?
            .map_err(msg)?;
        let (kind, location) = match &stored {
            Stored::File(p) => ("file", p.display().to_string()),
            Stored::Helper(h) => ("helper", format!("docker-credential-{h}")),
        };
        if self.json() {
            output::print_value(&output::Login {
                registry,
                stored: kind.into(),
                location,
            })?;
            return Ok(0);
        }
        match stored {
            Stored::File(_) => eprintln!(
                "sbx: saved in {location} (unencrypted; set credsStore there to keep it in a credential helper)"
            ),
            Stored::Helper(_) => eprintln!("sbx: saved with {location}"),
        }
        println!("Login Succeeded");
        Ok(0)
    }

    /// `sbx logout [SERVER]`.
    pub(super) fn logout(&self, server: Option<&str>) -> AppResult<u8> {
        let host = host_of(server)?;
        let removed = auth::erase(&crate::dirs::registry_auth(), &host).map_err(msg)?;
        if self.json() {
            output::print_value(&output::Logout {
                registry: host,
                removed,
            })?;
        } else if removed {
            println!("Removed login credentials for {host}");
        } else {
            println!("Not logged in to {host}");
        }
        Ok(0)
    }
}
