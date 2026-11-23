use std::{
    env,
    process::{Command, Stdio},
};

use anyhow::{Context, Result, ensure};
use iroh::{EndpointId, RelayUrl};
use iroh_ssh::invite::Code;

pub fn enabled(user: &str, peer: EndpointId) -> bool {
    // Only evaluate OpenSSH's Match/Include rules after an explicit ACL denial.
    // The original CLI also checked that all three standard streams were terminals.
    let result = Command::new("ssh")
        .args(["-G", "-o", "CanonicalizeHostname=no", "-l", user, "--"])
        .arg(peer.to_string())
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .output();
    result.is_ok_and(|output| {
        let config = String::from_utf8_lossy(&output.stdout);
        output.status.success() && interactive_config(&config)
    })
}

fn interactive_config(config: &str) -> bool {
    let mut human = false;
    for line in config.lines() {
        match line.split_once(' ') {
            Some(("batchmode", mode)) => human = mode == "no",
            Some(
                ("stdinnull", "yes") | ("requesttty", "no") | ("sessiontype", "none" | "subsystem"),
            ) => return false,
            Some(("remotecommand", command)) if command != "none" => return false,
            _ => {}
        }
    }
    human
}

pub struct Prompt {
    russian: bool,
}

impl Prompt {
    pub fn new() -> Self {
        let locale = ["LC_ALL", "LC_MESSAGES", "LANG"]
            .iter()
            .find_map(|name| env::var(name).ok().filter(|value| !value.is_empty()));
        Self {
            russian: locale.is_some_and(|s| s.starts_with("ru")),
        }
    }

    fn text<'a>(&self, ru: &'a str, en: &'a str) -> &'a str {
        if self.russian { ru } else { en }
    }

    pub fn read(
        &self,
        client: EndpointId,
        user: &str,
        peer: EndpointId,
        relay: &RelayUrl,
    ) -> Result<Code> {
        let help = self.text(
            "Получатель пока не разрешил вход с этого компьютера. Отправьте его владельцу свой Endpoint ID",
            "The receiver has not authorized this computer. Send your Endpoint ID to the receiver’s owner"
        );
        let ask = self.text(
            "Код или ссылка приглашения (Enter — отмена): ",
            "Invitation code or link (Enter to cancel): ",
        );
        // The proxy's stdout and stdin carry SSH packets. The password reader
        // uses the controlling terminal for both the prompt and hidden input.
        let text =
            rpassword::prompt_password(format!("{help}:\n{client}\n{ask}")).context(self.text(
                "Прочитать приглашение из терминала",
                "Read invitation from controlling terminal",
            ))?;
        ensure!(
            !text.trim().is_empty(),
            "{}",
            self.text("Ввод приглашения отменён", "Invitation entry cancelled")
        );
        let (_, _, code) =
            crate::invitation_cli::destination(&text, Some(peer), Some(user), Some(relay.clone()))?;
        Ok(code)
    }

    pub fn declined(&self) -> &'static str {
        self.text(
            "Не удалось принять приглашение. Проверьте код, срок его действия и ID, для которого его создали",
            "Could not redeem invitation. Check the code, its expiry and the client ID it was issued for"
        )
    }

    pub fn accepted(&self) -> &'static str {
        self.text(
            "Приглашение принято. Продолжаю вход по SSH.",
            "Invitation accepted. Continuing SSH.",
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_deny_prompts_only_when_ssh_allows_interaction() {
        assert!(interactive_config(
            "batchmode no\nrequesttty auto\nsessiontype default\n"
        ));
        for config in [
            "",
            "batchmode yes",
            "batchmode no\nstdinnull yes",
            "batchmode no\nrequesttty no",
            "batchmode no\nsessiontype subsystem",
            "batchmode no\nremotecommand uname -a",
        ] {
            assert!(!interactive_config(config));
        }
    }

    #[test]
    fn interactive_login_excludes_remote_commands_and_forwarding() {
        use clap::Parser;
        use iroh::SecretKey;
        let target = format!("pi@{}", SecretKey::generate().public());
        for flags in [vec![], vec!["-t"], vec!["-i", "/tmp/example-key"]] {
            let cli =
                crate::Cli::try_parse_from(["sshi"].into_iter().chain(flags).chain([&*target]))
                    .unwrap();
            assert!(cli.interactive_shell());
        }
        for flags in [
            vec!["-N"],
            vec!["-T"],
            vec!["-D", "1080"],
            vec!["-L", "8080:localhost:80"],
            vec!["-R", "9000:localhost:3000"],
        ] {
            let cli =
                crate::Cli::try_parse_from(["sshi"].into_iter().chain(flags).chain([&*target]))
                    .unwrap();
            assert!(!cli.interactive_shell());
        }
        let command = crate::Cli::try_parse_from(["sshi", &target, "hostname"]).unwrap();
        assert!(!command.interactive_shell());
    }

    #[test]
    fn a_login_invitation_must_match_the_selected_user_endpoint_and_relay() -> Result<()> {
        use iroh::SecretKey;
        use iroh_ssh::invite::{InvitationRoute, SharedInvitation};
        let peer = SecretKey::generate().public();
        let relay: RelayUrl = "https://relay.example/".parse()?;
        let route = InvitationRoute::new(peer, "pi", Some(relay.as_str()))?;
        let link = SharedInvitation {
            route,
            code: Code::generate(),
        }
        .to_string();
        let check = |user, id, relay| {
            crate::invitation_cli::destination(&link, Some(id), Some(user), Some(relay)).is_ok()
        };
        assert!(check("pi", peer, relay.clone()));
        assert!(!check("root", peer, relay.clone()));
        assert!(!check("pi", SecretKey::generate().public(), relay));
        assert!(!check("pi", peer, "https://other.example/".parse()?));
        Ok(())
    }
}
