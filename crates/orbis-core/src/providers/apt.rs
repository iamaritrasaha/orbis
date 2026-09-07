//! APT metadata provider using non-mutating `apt-cache` and `dpkg-query`.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    diagnostics::DiagnosticCheck,
    models::{Package, PackageKind, PackageSource, ProviderCapabilities, SourceInfo},
    process::{CommandRunner, CommandSpec, SharedRunner},
    providers::{Provider, ProviderError, execute, expect_success, short_timeout},
};

/// APT provider. Nala is detected as an optional frontend, but APT remains the source identity.
pub struct AptProvider {
    runner: SharedRunner,
}

impl AptProvider {
    /// Creates an APT provider around an injected command runner.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn available(&self) -> bool {
        self.runner.is_available("apt-cache")
    }

    fn installed_versions(&self, names: &[String]) -> BTreeMap<String, String> {
        if names.is_empty() || !self.runner.is_available("dpkg-query") {
            return BTreeMap::new();
        }
        let mut args =
            vec!["-W".to_owned(), "-f=${binary:Package}\t${Status}\t${Version}\n".to_owned()];
        args.extend(names.iter().cloned());
        let Ok(output) = execute(
            &self.runner,
            PackageSource::Apt,
            "read installed package state",
            CommandSpec::new("dpkg-query", args).with_timeout(short_timeout()),
        ) else {
            return BTreeMap::new();
        };
        if !output.success() {
            return BTreeMap::new();
        }
        output
            .stdout
            .lines()
            .filter_map(|line| {
                let mut fields = line.split('\t');
                let name = fields.next()?.to_owned();
                let status = fields.next()?;
                let version = fields.next()?.to_owned();
                status.contains("install ok installed").then_some((name, version))
            })
            .collect()
    }

    fn package_from_record(&self, record: &BTreeMap<String, String>) -> Package {
        let provider_id = value(record, "Package").unwrap_or_default();
        let summary = value(record, "Description-en")
            .or_else(|| value(record, "Description"))
            .and_then(|text| {
                text.lines()
                    .next()
                    .map(str::trim)
                    .filter(|line| !line.is_empty())
                    .map(str::to_owned)
            });
        let description = value(record, "Description-en").or_else(|| value(record, "Description"));
        let kind = classify(&provider_id, summary.as_deref(), description.as_deref());
        let mut metadata = BTreeMap::new();
        for field in ["Section", "Maintainer", "Source", "Installed-Size"] {
            if let Some(value) = record.get(field) {
                metadata.insert(field.to_ascii_lowercase(), value.clone());
            }
        }
        Package {
            source: PackageSource::Apt,
            name: provider_id.clone(),
            provider_id,
            version: value(record, "Version"),
            summary,
            description,
            installed: None,
            kind,
            origin: value(record, "Origin").or_else(|| value(record, "Section")),
            architecture: value(record, "Architecture"),
            homepage: value(record, "Homepage"),
            license: value(record, "License"),
            size_bytes: value(record, "Size").and_then(|size| size.parse().ok()),
            metadata,
        }
    }
}

impl Provider for AptProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Apt
    }

    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let nala = self.runner.is_available("nala");
        SourceInfo {
            source: PackageSource::Apt,
            available,
            state: if available { "ready".into() } else { "unavailable".into() },
            backend: Some(if nala {
                "Nala available; APT metadata backend".into()
            } else {
                "APT tools; Nala not installed".into()
            }),
            capabilities: capabilities(),
            notes: if available {
                vec!["Read-only metadata uses apt-cache and dpkg-query.".into()]
            } else {
                vec!["Install apt to enable Debian/Ubuntu package discovery.".into()]
            },
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Apt,
                program: "apt-cache".into(),
            });
        }
        let output = execute(
            &self.runner,
            PackageSource::Apt,
            "search package metadata",
            CommandSpec::new("apt-cache", ["--no-generate", "search", "--names-only", "--", query])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Apt, "search package metadata", output)?;
        let names_and_summaries: Vec<_> = output
            .stdout
            .lines()
            .filter_map(|line| line.split_once(" - "))
            .filter_map(|(name, summary)| {
                let name = name.trim();
                (!name.is_empty()).then_some((name.to_owned(), summary.trim().to_owned()))
            })
            .take(60)
            .collect();
        let names: Vec<String> = names_and_summaries.iter().map(|(name, _)| name.clone()).collect();
        let installed = self.installed_versions(&names);
        Ok(names_and_summaries
            .into_iter()
            .map(|(name, summary)| Package {
                source: PackageSource::Apt,
                provider_id: name.clone(),
                name: name.clone(),
                version: installed.get(&name).cloned(),
                summary: (!summary.is_empty()).then_some(summary.clone()),
                description: None,
                installed: Some(installed.contains_key(&name)),
                kind: classify(&name, Some(&summary), None),
                origin: None,
                architecture: None,
                homepage: None,
                license: None,
                size_bytes: None,
                metadata: BTreeMap::new(),
            })
            .collect())
    }

    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Apt,
                program: "apt-cache".into(),
            });
        }
        let output = execute(
            &self.runner,
            PackageSource::Apt,
            "read package information",
            CommandSpec::new(
                "apt-cache",
                ["--no-generate", "show", "--no-all-versions", "--", package_id],
            )
            .with_timeout(short_timeout()),
        )?;
        if !output.success() {
            let detail = format!("{} {}", output.stdout, output.stderr).to_ascii_lowercase();
            if detail.contains("no packages found") || detail.contains("unable to locate") {
                return Err(ProviderError::NotFound {
                    package_source: PackageSource::Apt,
                    query: package_id.into(),
                });
            }
        }
        let output = expect_success(PackageSource::Apt, "read package information", output)?;
        let record = parse_control_records(&output.stdout).into_iter().next().ok_or_else(|| {
            ProviderError::NotFound { package_source: PackageSource::Apt, query: package_id.into() }
        })?;
        let mut package = self.package_from_record(&record);
        if package.provider_id.is_empty() {
            return Err(ProviderError::Parse {
                package_source: PackageSource::Apt,
                operation: "read package information".into(),
                technical: "APT record did not contain a Package field".into(),
            });
        }
        let installed = self.installed_versions(std::slice::from_ref(&package.provider_id));
        package.installed = Some(installed.contains_key(&package.provider_id));
        if package.installed == Some(true) {
            package.version = installed.get(&package.provider_id).cloned().or(package.version);
        }
        Ok(package)
    }

    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Apt,
                "APT tools",
                "Install apt to enable APT discovery.",
            );
        }
        let result = execute(
            &self.runner,
            PackageSource::Apt,
            "check local package metadata",
            CommandSpec::new("apt-cache", ["--no-generate", "stats"]).with_timeout(short_timeout()),
        );
        match result.and_then(|output| {
            expect_success(PackageSource::Apt, "check local package metadata", output)
        }) {
            Ok(_) => DiagnosticCheck::passed(PackageSource::Apt, "APT metadata responds"),
            Err(error) => {
                DiagnosticCheck::failed(PackageSource::Apt, "APT metadata", &error.to_string())
            }
        }
    }
}

fn capabilities() -> ProviderCapabilities {
    ProviderCapabilities {
        search: true,
        info: true,
        installed_state: true,
        installed_list: true,
        mutations: false,
    }
}

fn value(record: &BTreeMap<String, String>, key: &str) -> Option<String> {
    record.get(key).cloned().filter(|value| !value.trim().is_empty())
}

fn parse_control_records(text: &str) -> Vec<BTreeMap<String, String>> {
    let mut records = Vec::new();
    let mut record: BTreeMap<String, String> = BTreeMap::new();
    let mut last_key: Option<String> = None;
    for line in text.lines() {
        if line.trim().is_empty() {
            if !record.is_empty() {
                records.push(record);
                record = BTreeMap::new();
                last_key = None;
            }
        } else if line.starts_with(char::is_whitespace) {
            if let Some(key) = &last_key {
                if let Some(value) = record.get_mut(key) {
                    value.push('\n');
                    value.push_str(line.trim_end());
                }
            }
        } else if let Some((key, value)) = line.split_once(':') {
            let key = key.to_owned();
            record.insert(key.clone(), value.trim().to_owned());
            last_key = Some(key);
        }
    }
    if !record.is_empty() {
        records.push(record);
    }
    records
}

fn classify(name: &str, summary: Option<&str>, description: Option<&str>) -> Option<PackageKind> {
    let lower =
        format!("{} {} {}", name, summary.unwrap_or_default(), description.unwrap_or_default())
            .to_ascii_lowercase();
    if name.ends_with("-dev")
        || name.ends_with("-devel")
        || lower.contains("development files")
        || lower.contains("development headers")
    {
        return Some(PackageKind::DevelopmentFiles);
    }
    if name.starts_with("lib") && !lower.contains("command line") && !lower.contains("monitor") {
        return Some(PackageKind::Library);
    }
    if lower.contains("command line")
        || lower.contains("terminal")
        || name == "btop"
        || name == "ffmpeg"
    {
        return Some(PackageKind::CliTool);
    }
    if lower.contains("daemon") || lower.contains("service") {
        return Some(PackageKind::Service);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multiline_apt_record() {
        let records = parse_control_records(
            "Package: libssl-dev\nVersion: 3.0\nDescription-en: Development files\n for OpenSSL.\n\n",
        );
        assert_eq!(records[0]["Package"], "libssl-dev");
        assert_eq!(records[0]["Description-en"], "Development files\n for OpenSSL.");
    }

    #[test]
    fn classifies_development_files_conservatively() {
        assert_eq!(
            classify("libssl-dev", Some("Secure Sockets Layer toolkit - development files"), None),
            Some(PackageKind::DevelopmentFiles)
        );
    }
}
