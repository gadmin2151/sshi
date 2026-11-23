use std::{env, fs, time::Duration};

use anyhow::{Context, Result, ensure};
use semver::Version;
use serde::{Deserialize, Serialize};

const LATEST_API: &str = "https://api.github.com/repos/gadmin2151/sshi/releases/latest";
const RELEASES: &str = "https://github.com/gadmin2151/sshi/releases";

#[derive(Clone, Debug, Serialize)]
pub struct UpdateInfo {
    pub current: String,
    pub latest: String,
    pub available: bool,
    pub release_url: String,
    pub download_url: Option<String>,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
    draft: bool,
    prerelease: bool,
    html_url: String,
    assets: Vec<Asset>,
}

#[derive(Deserialize)]
struct Asset {
    name: String,
    state: String,
    browser_download_url: String,
}

impl Release {
    fn info(self, current: &str, os: &str, arch: &str, os_release: &str) -> Result<UpdateInfo> {
        ensure!(
            !self.draft && !self.prerelease,
            "GitHub did not return a stable release"
        );
        let latest = self
            .tag_name
            .strip_prefix('v')
            .context("invalid SSHI release tag")?;
        let version = Version::parse(latest).context("invalid SSHI release version")?;
        ensure!(version.pre.is_empty(), "GitHub returned a prerelease tag");
        let release_url = format!("{RELEASES}/tag/{}", self.tag_name);
        ensure!(
            self.html_url == release_url,
            "unexpected GitHub release URL"
        );
        let download_url = asset_name(os, arch, os_release, latest).and_then(|name| {
            let url = format!("{RELEASES}/download/{}/{name}", self.tag_name);
            self.assets
                .iter()
                .any(|asset| {
                    asset.name == name
                        && asset.state == "uploaded"
                        && asset.browser_download_url == url
                })
                .then_some(url)
        });
        Ok(UpdateInfo {
            current: current.into(),
            latest: latest.into(),
            available: version.cmp_precedence(&Version::parse(current)?).is_gt(),
            release_url,
            download_url,
        })
    }
}

#[derive(Clone, Copy)]
enum LinuxPackage {
    Deb,
    Rpm,
    Arch,
}

fn linux_package(os_release: &str) -> Option<LinuxPackage> {
    let field = |key: &str| {
        os_release.lines().find_map(|line| {
            let (name, value) = line.trim().split_once('=')?;
            (name == key).then(|| value.trim().trim_matches(['"', '\'']))
        })
    };
    field("ID")
        .into_iter()
        .chain(field("ID_LIKE"))
        .flat_map(str::split_ascii_whitespace)
        .find_map(|id| match id {
            "debian" | "ubuntu" | "raspbian" => Some(LinuxPackage::Deb),
            "fedora" | "rhel" | "centos" | "suse" | "opensuse" => Some(LinuxPackage::Rpm),
            "arch" => Some(LinuxPackage::Arch),
            _ => None,
        })
}

fn asset_name(os: &str, arch: &str, os_release: &str, version: &str) -> Option<String> {
    match (os, arch) {
        ("windows", "x86_64") => Some(format!("sshi-{version}-windows-x64.exe")),
        ("macos", "aarch64") => Some(format!("sshi-{version}-macos-arm64.pkg")),
        ("linux", arch) => match (linux_package(os_release)?, arch) {
            (LinuxPackage::Deb, "x86_64") => Some(format!("sshi_{version}_amd64.deb")),
            (LinuxPackage::Deb, "aarch64") => Some(format!("sshi_{version}_arm64.deb")),
            (LinuxPackage::Deb, "armhf") => Some(format!("sshi_{version}_armhf.deb")),
            (LinuxPackage::Rpm, "x86_64" | "aarch64") => {
                Some(format!("sshi-{version}-1.{arch}.rpm"))
            }
            (LinuxPackage::Arch, "x86_64") => Some(format!("sshi-{version}-1-x86_64.pkg.tar.zst")),
            _ => None,
        },
        _ => None,
    }
}

pub async fn check() -> Result<UpdateInfo> {
    // reqwest's rustls-no-provider feature uses the process provider, even when
    // this is the first operation and no Iroh endpoint has been initialized.
    if rustls::crypto::CryptoProvider::get_default().is_none() {
        let _ = rustls::crypto::ring::default_provider().install_default();
    }
    let client = reqwest::Client::builder()
        .https_only(true)
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(Duration::from_secs(4))
        .timeout(Duration::from_secs(10))
        .user_agent(concat!("SSHI/", env!("CARGO_PKG_VERSION")))
        .build()?;
    let mut response = client
        .get(LATEST_API)
        .header(reqwest::header::ACCEPT, "application/vnd.github+json")
        .send()
        .await
        .context("could not reach GitHub to check for SSHI updates")?;
    ensure!(
        response.status().is_success(),
        "GitHub update check returned {}",
        response.status()
    );
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        ensure!(
            chunk.len() <= 1_048_576 - body.len(),
            "GitHub release response is too large"
        );
        body.extend_from_slice(&chunk);
    }
    let release: Release =
        serde_json::from_slice(&body).context("invalid GitHub release response")?;
    let os_release = if cfg!(target_os = "linux") {
        fs::read_to_string("/etc/os-release")
            .or_else(|_| fs::read_to_string("/usr/lib/os-release"))
            .unwrap_or_default()
    } else {
        String::new()
    };
    let arch = if cfg!(all(target_arch = "arm", target_abi = "eabihf")) {
        "armhf"
    } else {
        env::consts::ARCH
    };
    release.info(
        env!("CARGO_PKG_VERSION"),
        env::consts::OS,
        arch,
        &os_release,
    )
}

pub async fn print_check(json: bool) -> Result<()> {
    let info = check().await?;
    if json {
        println!("{}", serde_json::to_string(&info)?);
    } else if info.available {
        println!(
            "SSHI {} is available (installed {}).",
            info.latest, info.current
        );
        println!(
            "{}",
            info.download_url.as_deref().unwrap_or(&info.release_url)
        );
    } else {
        println!(
            "Installed SSHI {}. Latest stable release {}.",
            info.current, info.latest
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn release(tag: &str, asset: &str) -> Release {
        serde_json::from_value(serde_json::json!({
            "tag_name": tag, "draft": false, "prerelease": false,
            "html_url": format!("{RELEASES}/tag/{tag}"),
            "assets": [{"name": asset, "state": "uploaded", "browser_download_url": format!("{RELEASES}/download/{tag}/{asset}")}]
        })).unwrap()
    }

    #[test]
    fn matches_packages_in_the_supported_release_matrix() {
        let version = "0.10.0";
        for (os, arch, distribution, expected) in [
            ("linux", "x86_64", "ID=ubuntu", "sshi_0.10.0_amd64.deb"),
            ("linux", "aarch64", "ID=debian", "sshi_0.10.0_arm64.deb"),
            ("linux", "armhf", "ID=raspbian", "sshi_0.10.0_armhf.deb"),
            (
                "linux",
                "x86_64",
                "ID=rocky\nID_LIKE=\"rhel centos fedora\"",
                "sshi-0.10.0-1.x86_64.rpm",
            ),
            ("linux", "aarch64", "ID=fedora", "sshi-0.10.0-1.aarch64.rpm"),
            (
                "linux",
                "x86_64",
                "ID_LIKE=arch\nID=endeavouros",
                "sshi-0.10.0-1-x86_64.pkg.tar.zst",
            ),
            ("windows", "x86_64", "", "sshi-0.10.0-windows-x64.exe"),
            ("macos", "aarch64", "", "sshi-0.10.0-macos-arm64.pkg"),
        ] {
            assert_eq!(
                asset_name(os, arch, distribution, version).as_deref(),
                Some(expected)
            );
            let info = release("v0.10.0", expected)
                .info("0.9.0", os, arch, distribution)
                .unwrap();
            assert!(info.available);
            assert_eq!(
                info.download_url.unwrap(),
                format!("{RELEASES}/download/v0.10.0/{expected}")
            );
        }
    }

    #[test]
    fn checks_stability_and_semver_precedence() {
        for current in ["0.10.0", "0.10.0+local", "0.11.0"] {
            let info = release("v0.10.0", "")
                .info(current, "linux", "x86_64", "")
                .unwrap();
            assert!(!info.available);
        }
        for tag in ["0.10.0", "v0.10.0-beta.1", "v../main"] {
            assert!(
                release(tag, "")
                    .info("0.1.0", "macos", "aarch64", "")
                    .is_err()
            );
        }
        for (draft, prerelease) in [(true, false), (false, true)] {
            let mut release = release("v0.10.0", "");
            release.draft = draft;
            release.prerelease = prerelease;
            assert!(release.info("0.1.0", "macos", "aarch64", "").is_err());
        }
    }

    #[test]
    fn only_returns_urls_for_official_releases_and_available_packages() {
        let asset = "sshi_0.10.0_amd64.deb";
        let linux =
            |release: Release, distro: &str| release.info("0.9.0", "linux", "x86_64", distro);
        for mut release in [release("v0.10.0", asset), release("v0.10.0", "foreign.deb")] {
            release.assets[0].browser_download_url = "https://example.com/download".into();
            assert!(linux(release, "ID=debian").unwrap().download_url.is_none());
        }
        let mut invalid_url = release("v0.10.0", asset);
        invalid_url.html_url = "https://github.com/other/sshi/releases/tag/v0.10.0".into();
        assert!(linux(invalid_url, "ID=debian").is_err());
        let mut missing = release("v0.10.0", asset);
        missing.assets[0].state = "new".into();
        assert!(linux(missing, "ID=debian").unwrap().download_url.is_none());
        let unknown = linux(release("v0.10.0", asset), "ID=alpine").unwrap();
        assert_eq!(
            serde_json::to_value(unknown).unwrap(),
            serde_json::json!({
                "current": "0.9.0", "latest": "0.10.0", "available": true,
                "release_url": format!("{RELEASES}/tag/v0.10.0"), "download_url": null
            })
        );
    }
}
