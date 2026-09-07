//! Snap provider using the read-only `snap find`, `snap list`, and `snap info` commands.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    diagnostics::DiagnosticCheck,
    models::{Package, PackageKind, PackageSource, ProviderCapabilities, SourceInfo},
    process::{CommandRunner, CommandSpec, SharedRunner},
    providers::{
        Provider, ProviderError, TransactionProvider, execute, expect_success, short_timeout,
    },
    transaction::{
        InstallScope, OperationAction, OperationPlan, OperationRequest, PlanCompleteness,
        PlanConfidence, PrivilegeRequirement, ProviderOperation, TransactionError,
        VerificationResult, WarningLevel, base_risk, target_change,
    },
};

/// Snap provider.
pub struct SnapProvider {
    runner: SharedRunner,
}

impl TransactionProvider for SnapProvider {
    fn plan_transaction(
        &self,
        request: &OperationRequest,
        target: &Package,
    ) -> Result<OperationPlan, TransactionError> {
        if request.scope == Some(InstallScope::User) {
            return Err(TransactionError::InvalidRequest(
                "Snap packages use the system scope; omit `--scope user`".into(),
            ));
        }
        validate_package_id(&target.provider_id)?;
        let installed = target.installed == Some(true);
        match (request.action, installed) {
            (OperationAction::Install, true) => {
                return Err(TransactionError::Planning(format!(
                    "Snap `{}` is already installed",
                    target.provider_id
                )));
            }
            (OperationAction::Remove, false) => {
                return Err(TransactionError::Planning(format!(
                    "Snap `{}` is not installed",
                    target.provider_id
                )));
            }
            _ => {}
        }
        let record = self.info_record(&target.provider_id)?;
        let installed_map = self.installed().unwrap_or_default();
        let mut resolved_target =
            package_from_record(record.clone(), Some(&installed_map), &target.provider_id)
                .map_err(|error| TransactionError::Planning(error.to_string()))?;
        resolved_target.installed = Some(installed);
        if let Some(channel) = &request.channel {
            validate_channel(channel)?;
            if let Some(version) = channel_version_for(record.get("channels"), channel) {
                resolved_target.version = Some(version.to_owned());
            }
            resolved_target.metadata.insert("requested_channel".into(), channel.clone());
        }
        let mut plan = OperationPlan::new(request.action, resolved_target, InstallScope::System);
        plan.privilege = PrivilegeRequirement::Administrator;
        plan.completeness = PlanCompleteness::Partial;
        plan.confidence = PlanConfidence::Medium;
        plan.authoritative_simulation = false;
        plan.changes.push(target_change(&plan));
        plan.risk = base_risk(request.action, target.kind);
        if request.action == OperationAction::Install {
            plan.warnings.push(crate::transaction::PlanWarning {
                level: WarningLevel::Caution,
                message: "Snap resolves store metadata and confinement details at commit time; this provider has no zero-action dependency simulation.".into(),
            });
            if request.channel.is_none() {
                plan.warnings.push(crate::transaction::PlanWarning {
                    level: WarningLevel::Info,
                    message: "No channel was specified; Snap's normal latest/stable channel will be used.".into(),
                });
            }
        } else {
            plan.warnings.push(crate::transaction::PlanWarning {
                level: WarningLevel::Caution,
                message:
                    "Snap normally retains a removable data snapshot; Orbis does not use `--purge`."
                        .into(),
            });
        }
        Ok(plan)
    }

    fn provider_operation(
        &self,
        plan: &OperationPlan,
    ) -> Result<ProviderOperation, TransactionError> {
        validate_package_id(&plan.target.provider_id)?;
        Ok(ProviderOperation::Snap {
            action: plan.action,
            package_id: plan.target.provider_id.clone(),
            channel: plan.target.metadata.get("requested_channel").cloned(),
        })
    }

    fn verify_transaction(
        &self,
        plan: &OperationPlan,
    ) -> Result<VerificationResult, TransactionError> {
        let installed = self.installed()?.contains_key(&plan.target.provider_id);
        let expected = plan.action == OperationAction::Install;
        Ok(if installed == expected {
            VerificationResult::Verified
        } else {
            VerificationResult::Failed
        })
    }
}

fn validate_package_id(package_id: &str) -> Result<(), TransactionError> {
    if package_id.is_empty()
        || package_id.starts_with('-')
        || package_id.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, ';' | '&' | '|')
        })
    {
        return Err(TransactionError::InvalidRequest(
            "Snap names must be exact names without whitespace or option characters".into(),
        ));
    }
    Ok(())
}

fn validate_channel(channel: &str) -> Result<(), TransactionError> {
    if channel.is_empty()
        || channel.starts_with('-')
        || channel.chars().any(|character| {
            character.is_control()
                || character.is_whitespace()
                || matches!(character, ';' | '&' | '|')
        })
    {
        return Err(TransactionError::InvalidRequest(
            "Snap channels must be names without whitespace or option characters".into(),
        ));
    }
    Ok(())
}

impl SnapProvider {
    /// Creates a Snap provider around an injected command runner.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn available(&self) -> bool {
        self.runner.is_available("snap")
    }

    fn installed(&self) -> Result<BTreeMap<String, String>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Snap,
            "read installed Snap state",
            CommandSpec::new("snap", ["list"]).with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Snap, "read installed Snap state", output)?;
        Ok(output.stdout.lines().skip(1).filter_map(parse_installed_line).collect())
    }

    fn info_record(&self, name: &str) -> Result<BTreeMap<String, String>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Snap,
            "read Snap information",
            CommandSpec::new("snap", ["info", name]).with_timeout(short_timeout()),
        )?;
        if !output.success() {
            let detail = output.stderr.to_ascii_lowercase();
            if detail.contains("not found")
                || detail.contains("no snap")
                || detail.contains("cannot find")
            {
                return Err(ProviderError::NotFound {
                    package_source: PackageSource::Snap,
                    query: name.into(),
                });
            }
        }
        let output = expect_success(PackageSource::Snap, "read Snap information", output)?;
        Ok(parse_info(&output.stdout))
    }
}

impl Provider for SnapProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Snap
    }

    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        SourceInfo {
            source: PackageSource::Snap,
            available,
            state: if available { "ready".into() } else { "unavailable".into() },
            backend: Some("snap CLI / snapd".into()),
            capabilities: capabilities(),
            notes: if available {
                vec!["Snap metadata comes from the local snapd client and store queries.".into()]
            } else {
                vec!["Install snapd to enable Snap discovery.".into()]
            },
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Snap,
                program: "snap".into(),
            });
        }
        let installed = self.installed().unwrap_or_default();
        let output = execute(
            &self.runner,
            PackageSource::Snap,
            "search the Snap Store",
            CommandSpec::new("snap", ["find", query]).with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Snap, "search the Snap Store", output)?;
        Ok(output
            .stdout
            .lines()
            .skip(1)
            .filter_map(parse_find_line)
            .map(|(name, version, publisher, summary)| Package {
                source: PackageSource::Snap,
                provider_id: name.clone(),
                name: name.clone(),
                version: Some(version),
                summary: Some(summary.clone()),
                description: None,
                installed: Some(installed.contains_key(&name)),
                kind: classify(&name, &summary, None),
                origin: Some(publisher.clone()),
                architecture: None,
                homepage: None,
                license: None,
                size_bytes: None,
                metadata: BTreeMap::from([("publisher".into(), publisher)]),
            })
            .take(60)
            .collect())
    }

    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Snap,
                program: "snap".into(),
            });
        }
        let record = self.info_record(package_id)?;
        let installed = self.installed().unwrap_or_default();
        package_from_record(record, Some(&installed), package_id)
    }

    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Snap,
                "Snap CLI",
                "Install snapd to enable Snap discovery.",
            );
        }
        let result = execute(
            &self.runner,
            PackageSource::Snap,
            "check snapd responsiveness",
            CommandSpec::new("snap", ["version"]).with_timeout(short_timeout()),
        );
        match result.and_then(|output| {
            expect_success(PackageSource::Snap, "check snapd responsiveness", output)
        }) {
            Ok(_) => DiagnosticCheck::passed(PackageSource::Snap, "snapd responds"),
            Err(error) => DiagnosticCheck::failed(PackageSource::Snap, "snapd", &error.to_string()),
        }
    }
}

fn capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        search: true,
        info: true,
        installed_state: true,
        installed_list: true,
        mutations: true,
    }
}

fn parse_find_line(line: &str) -> Option<(String, String, String, String)> {
    let mut fields = line.split_whitespace();
    let name = fields.next()?.to_owned();
    let version = fields.next()?.to_owned();
    let publisher = fields.next()?.to_owned();
    let _notes = fields.next()?;
    let summary = fields.collect::<Vec<_>>().join(" ");
    (!name.is_empty() && !summary.is_empty()).then_some((name, version, publisher, summary))
}

fn parse_installed_line(line: &str) -> Option<(String, String)> {
    let mut fields = line.split_whitespace();
    let name = fields.next()?.to_owned();
    let version = fields.next()?.to_owned();
    Some((name, version))
}

fn parse_info(text: &str) -> BTreeMap<String, String> {
    let mut fields = BTreeMap::new();
    let mut current: Option<String> = None;
    for line in text.lines() {
        if !line.starts_with(char::is_whitespace) {
            if let Some((key, value)) = line.split_once(':') {
                let key = key.trim().to_owned();
                fields.insert(key.clone(), value.trim().trim_end_matches('|').trim().to_owned());
                current = Some(key);
            }
        } else if let Some(key) = &current {
            let value = line.trim();
            if !value.is_empty() {
                let entry = fields.entry(key.clone()).or_default();
                if !entry.is_empty() {
                    entry.push('\n');
                }
                entry.push_str(value);
            }
        }
    }
    fields
}

fn package_from_record(
    record: BTreeMap<String, String>,
    installed: Option<&BTreeMap<String, String>>,
    fallback_id: &str,
) -> Result<Package, ProviderError> {
    let provider_id = record.get("name").cloned().unwrap_or_else(|| fallback_id.to_owned());
    let summary = record.get("summary").cloned().filter(|value| !value.is_empty());
    let description = record.get("description").cloned().filter(|value| !value.is_empty());
    let version = record
        .get("installed")
        .and_then(|value| value.split_whitespace().next())
        .or_else(|| channel_version(record.get("channels")))
        .map(str::to_owned);
    let mut metadata = BTreeMap::new();
    for key in ["publisher", "tracking", "store-url", "contact", "license"] {
        if let Some(value) = record.get(key).filter(|value| !value.is_empty()) {
            metadata.insert(key.to_owned(), value.clone());
        }
    }
    let publisher = record.get("publisher").cloned();
    Ok(Package {
        source: PackageSource::Snap,
        provider_id: provider_id.clone(),
        name: provider_id.clone(),
        version: version
            .or_else(|| installed.and_then(|packages| packages.get(&provider_id).cloned())),
        summary: summary.clone(),
        description,
        installed: installed.map(|packages| packages.contains_key(&provider_id)),
        kind: classify(
            &provider_id,
            summary.as_deref().unwrap_or_default(),
            record.get("description").map(String::as_str),
        ),
        origin: publisher,
        architecture: None,
        homepage: record.get("store-url").cloned(),
        license: record.get("license").cloned(),
        size_bytes: record
            .get("installed")
            .and_then(|value| value.split_whitespace().nth(2))
            .and_then(parse_size),
        metadata,
    })
}

fn channel_version(channels: Option<&String>) -> Option<&str> {
    channels?.lines().find_map(|line| {
        let value = line.strip_prefix("latest/stable:")?.split_whitespace().next()?;
        (!value.is_empty() && value != "^").then_some(value)
    })
}

fn channel_version_for<'a>(channels: Option<&'a String>, requested: &str) -> Option<&'a str> {
    let channels = channels?;
    channels.lines().find_map(|line| {
        let (channel, rest) = line.split_once(':')?;
        let version = rest.split_whitespace().next()?;
        (channel.trim() == requested && !version.is_empty() && version != "^").then_some(version)
    })
}

fn parse_size(value: &str) -> Option<u64> {
    let (number, suffix) = value.split_at(value.len().saturating_sub(2));
    let multiplier = match suffix.to_ascii_uppercase().as_str() {
        "KB" => 1_000_f64,
        "MB" => 1_000_000_f64,
        "GB" => 1_000_000_000_f64,
        _ => return value.parse().ok(),
    };
    number.parse::<f64>().ok().map(|number| (number * multiplier) as u64)
}

fn classify(name: &str, summary: &str, description: Option<&str>) -> Option<PackageKind> {
    let lower =
        format!("{name} {summary} {}", description.unwrap_or_default()).to_ascii_lowercase();
    if lower.contains("daemon") || lower.contains("service") {
        Some(PackageKind::Service)
    } else if lower.contains("terminal")
        || lower.contains("command line")
        || name == "btop"
        || name == "ffmpeg"
    {
        Some(PackageKind::CliTool)
    } else {
        Some(PackageKind::Application)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_snap_search_table_with_spaced_summary() {
        let item = parse_find_line(
            "btop 1.4.7 kz6fittycent - Resource monitor that shows usage and stats",
        )
        .expect("row");
        assert_eq!(item.0, "btop");
        assert_eq!(item.3, "Resource monitor that shows usage and stats");
    }

    #[test]
    fn parses_snap_info_description_and_installed_version() {
        let record = parse_info(
            "name: btop\nsummary: Resource monitor\ndescription: |\n  Watches CPU and memory.\ninstalled: 1.4.7 (1004) 2.03MB -\n",
        );
        assert_eq!(record["name"], "btop");
        assert!(record["description"].contains("Watches CPU"));
        assert_eq!(record["installed"], "1.4.7 (1004) 2.03MB -");
    }
}
