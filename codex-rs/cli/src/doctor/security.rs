#[cfg(target_os = "macos")]
use std::ffi::OsStr;
#[cfg(target_os = "macos")]
use std::process::Output;
#[cfg(target_os = "macos")]
use std::process::Stdio;
#[cfg(target_os = "macos")]
use std::time::Duration;

#[cfg(target_os = "macos")]
use tokio::process::Command;
#[cfg(target_os = "macos")]
use tokio::time::timeout;

use super::CheckStatus;
use super::DoctorCheck;
use super::DoctorIssue;

#[cfg(target_os = "macos")]
const PRODUCT_QUERY_TIMEOUT: Duration = Duration::from_secs(5);
#[cfg(target_os = "macos")]
const MAX_PRODUCT_OUTPUT_BYTES: usize = 64 * 1024;

pub(super) enum EndpointInspection {
    Complete(Vec<&'static str>),
    #[cfg(test)]
    Partial(Vec<&'static str>),
    #[cfg(any(target_os = "macos", test))]
    Unavailable,
}

pub(super) async fn check() -> DoctorCheck {
    endpoint_check(endpoint_products().await)
}

pub(super) fn endpoint_check(inspection: EndpointInspection) -> DoctorCheck {
    let (products, visibility_incomplete) = match inspection {
        EndpointInspection::Complete(products) => (products, false),
        #[cfg(test)]
        EndpointInspection::Partial(products) => (products, true),
        #[cfg(any(target_os = "macos", test))]
        EndpointInspection::Unavailable => {
            return DoctorCheck::new(
                "security.endpoint",
                "security",
                CheckStatus::Warning,
                "endpoint protection inspection unavailable",
            )
            .detail("endpoint products: unavailable");
        }
    };

    if products.is_empty() {
        let (summary, detail) = if cfg!(any(target_os = "macos")) {
            ("no supported endpoint protection detected", "none detected")
        } else {
            (
                "endpoint protection is not inspected on this platform",
                "not inspected on this platform",
            )
        };
        return DoctorCheck::new("security.endpoint", "security", CheckStatus::Ok, summary)
            .detail(format!("endpoint products: {detail}"));
    }

    let targets = if false {
        "signed Codex app; codex.exe; codex-windows-sandbox-setup.exe; codex-command-runner.exe"
    } else if cfg!(target_os = "macos") {
        "signing team 2DC432GLL2 plus the installed Codex app identity, signed codex agent, and required helpers"
    } else {
        "verified Codex app, codex agent, and required helpers"
    };
    let mut check = DoctorCheck::new(
        "security.endpoint",
        "security",
        CheckStatus::Warning,
        "endpoint protection detected; Codex exclusions are unverified",
    )
    .detail(format!("endpoint products: {}", products.join(", ")))
    .detail(format!("exclusion targets: {targets}"))
    .detail("Codex exclusions: not verified")
    .remediation(
        "ask your security administrator to verify Codex exclusions and required helper allowances",
    );
    if visibility_incomplete {
        check = check.detail("additional endpoint products: unavailable");
    }

    for product in products {
        let remedy = match product {
            "CrowdStrike Falcon" => {
                "CrowdStrike Falcon: Add a certificate or IOA exclusion for Codex. If sensor overhead continues, exclude the Codex agent from sensor visibility. Keep monitoring descendant processes."
            }
            "BeyondTrust Privilege Management" => {
                "BeyondTrust: Remove Codex from application blocking rules. Add allow rules for Codex helper executables. Do not grant administrator privileges."
            }
            "Microsoft Defender" => {
                "Microsoft Defender: Add a certificate or executable-path exclusion for Codex and its helpers. If Attack Surface Reduction blocks Codex, add a rule exclusion. If Controlled Folder Access blocks Codex, allow the app."
            }
            "SentinelOne" => {
                "SentinelOne: Add a signer, file-hash, or executable-path exclusion for Codex and its helpers."
            }
            "Jamf Protect" => {
                "Jamf Protect: Add an Override Threat Prevention exception for Codex app and helper signing identities. If analytics cause delays, add an Ignore System Events for Analytics exception."
            }
            _ => {
                "Add an exclusion for Codex and its helpers. Use the endpoint product instructions."
            }
        };
        check = check.issue(
            DoctorIssue::new(
                CheckStatus::Warning,
                format!("{product} can interfere with Codex. Verify Codex exclusions."),
            )
            .measured("not verified")
            .expected("Codex application and helper exclusions")
            .remedy(remedy)
            .field("Codex exclusions"),
        );
    }

    check
}

async fn endpoint_products() -> EndpointInspection {
    #[cfg(target_os = "macos")]
    {
        const PRODUCTS: &[(&str, &str, &str)] = &[
            (
                "CrowdStrike Falcon",
                "X9E956P446",
                "com.crowdstrike.falcon.Agent",
            ),
            (
                "BeyondTrust Privilege Management",
                "2ZS8T6NYB8",
                "com.beyondtrust.endpointsecurity",
            ),
            (
                "Microsoft Defender",
                "UBF8T346G9",
                "com.microsoft.wdav.epsext",
            ),
            (
                "SentinelOne",
                "4AYE5J54KN",
                "com.sentinelone.network-monitoring",
            ),
            (
                "Jamf Protect",
                "483DWKW443",
                "com.jamf.protect.security-extension",
            ),
        ];
        let Some(output) = product_command("/usr/bin/systemextensionsctl", &["list"])
            .await
            .filter(|output| output.status.success())
        else {
            return EndpointInspection::Unavailable;
        };
        EndpointInspection::Complete(
            String::from_utf8_lossy(&output.stdout)
                .lines()
                .filter_map(|line| {
                    let mut columns = line.split_whitespace();
                    if columns.next() != Some("*") || columns.next() != Some("*") {
                        return None;
                    }
                    let team = columns.next()?;
                    let bundle = columns.next()?;
                    PRODUCTS.iter().find_map(|&(name, signer, identifier)| {
                        (team == signer && bundle == identifier).then_some(name)
                    })
                })
                .collect(),
        )
    }

    #[cfg(not(target_os = "macos"))]
    {
        EndpointInspection::Complete(Vec::new())
    }
}

#[cfg(target_os = "macos")]
async fn product_command(program: impl AsRef<OsStr>, args: &[&str]) -> Option<Output> {
    let mut command = Command::from(codex_utils_process::background_command(program));
    command
        .args(args)
        .stdin(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    timeout(PRODUCT_QUERY_TIMEOUT, command.output())
        .await
        .ok()?
        .ok()
        .filter(|output| output.stdout.len() <= MAX_PRODUCT_OUTPUT_BYTES)
}

#[cfg(test)]
#[path = "security_tests.rs"]
mod tests;
