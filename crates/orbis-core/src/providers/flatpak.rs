//! Flatpak provider using documented column-based output.

use std::{collections::BTreeMap, sync::Arc};

use crate::{
    diagnostics::DiagnosticCheck,
    models::{Package, PackageKind, PackageSource, ProviderCapabilities, SourceInfo},
    process::{CommandRunner, CommandSpec, SharedRunner},
    providers::{Provider, ProviderError, execute, expect_success, short_timeout},
};

/// Flatpak provider.
pub struct FlatpakProvider {
    runner: SharedRunner,
}

impl FlatpakProvider {
    /// Creates a Flatpak provider around an injected command runner.
    pub fn new(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn available(&self) -> bool {
        self.runner.is_available("flatpak")
    }

    fn installed(&self) -> Result<BTreeMap<String, String>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read installed Flatpak state",
            CommandSpec::new(
                "flatpak",
                [
                    "list",
                    "--app",
                    "--columns=application,version,branch,origin,arch,name,description",
                ],
            )
            .with_timeout(short_timeout()),
        )?;
        let output =
            expect_success(PackageSource::Flatpak, "read installed Flatpak state", output)?;
        Ok(output
            .stdout
            .lines()
            .filter_map(parse_columns)
            .filter_map(|fields| {
                let id = fields.first()?.to_owned();
                let version = fields.get(1).cloned().unwrap_or_default();
                Some((id, version))
            })
            .collect())
    }

    fn remotes(&self) -> Result<Vec<String>, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read Flatpak remotes",
            CommandSpec::new("flatpak", ["remotes", "--columns=name"])
                .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Flatpak, "read Flatpak remotes", output)?;
        Ok(output
            .stdout
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect())
    }

    fn info_columns(&self, package_id: &str) -> Result<Package, ProviderError> {
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "read Flatpak application information",
            CommandSpec::new(
                "flatpak",
                ["info", "--columns=application,name,description,version,branch,origin,arch,homepage,installed-size", package_id],
            )
            .with_timeout(short_timeout()),
        )?;
        let output =
            expect_success(PackageSource::Flatpak, "read Flatpak application information", output)?;
        let fields =
            parse_columns(output.stdout.lines().next().unwrap_or_default()).ok_or_else(|| {
                ProviderError::NotFound {
                    package_source: PackageSource::Flatpak,
                    query: package_id.into(),
                }
            })?;
        package_from_fields(fields, None)
    }
}

impl Provider for FlatpakProvider {
    fn source(&self) -> PackageSource {
        PackageSource::Flatpak
    }

    fn source_info(&self) -> SourceInfo {
        let available = self.available();
        let notes = if !available {
            vec!["Install Flatpak to discover applications from configured remotes.".into()]
        } else {
            match self.remotes() {
                Ok(remotes) if remotes.is_empty() => {
                    vec!["No Flatpak remotes are configured.".into()]
                }
                Ok(remotes) => vec![format!("Remotes: {}", remotes.join(", "))],
                Err(error) => vec![format!("Remotes could not be inspected: {error}")],
            }
        };
        SourceInfo {
            source: PackageSource::Flatpak,
            available,
            state: if available { "ready".into() } else { "unavailable".into() },
            backend: Some("Flatpak CLI".into()),
            capabilities: capabilities(),
            notes,
        }
    }

    fn search(&self, query: &str) -> Result<Vec<Package>, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        let installed = self.installed().unwrap_or_default();
        let output = execute(
            &self.runner,
            PackageSource::Flatpak,
            "search Flatpak applications",
            CommandSpec::new(
                "flatpak",
                ["search", "--columns=application,name,description,version,branch,remotes", query],
            )
            .with_timeout(short_timeout()),
        )?;
        let output = expect_success(PackageSource::Flatpak, "search Flatpak applications", output)?;
        Ok(output
            .stdout
            .lines()
            .filter_map(parse_columns)
            .filter_map(|fields| package_from_fields(fields, Some(&installed)).ok())
            .take(60)
            .collect())
    }

    fn info(&self, package_id: &str) -> Result<Package, ProviderError> {
        if !self.available() {
            return Err(ProviderError::Unavailable {
                package_source: PackageSource::Flatpak,
                program: "flatpak".into(),
            });
        }
        match self.info_columns(package_id) {
            Ok(mut package) => {
                let installed = self.installed().unwrap_or_default();
                package.installed = Some(installed.contains_key(&package.provider_id));
                Ok(package)
            }
            Err(ProviderError::Command { .. }) | Err(ProviderError::NotFound { .. }) => {
                let candidates = self.search(package_id)?;
                let candidate = candidates
                    .into_iter()
                    .find(|package| {
                        package.provider_id == package_id
                            || package.name.eq_ignore_ascii_case(package_id)
                    })
                    .ok_or_else(|| ProviderError::NotFound {
                        package_source: PackageSource::Flatpak,
                        query: package_id.into(),
                    })?;
                self.info_columns(&candidate.provider_id)
            }
            Err(error) => Err(error),
        }
    }

    fn diagnostic(&self) -> DiagnosticCheck {
        if !self.available() {
            return DiagnosticCheck::failed(
                PackageSource::Flatpak,
                "Flatpak CLI",
                "Install Flatpak to enable Flatpak discovery.",
            );
        }
        match self.remotes() {
            Ok(_) => {
                DiagnosticCheck::passed(PackageSource::Flatpak, "Flatpak CLI and remotes respond")
            }
            Err(error) => DiagnosticCheck::failed(
                PackageSource::Flatpak,
                "Flatpak remotes",
                &error.to_string(),
            ),
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

fn parse_columns(line: &str) -> Option<Vec<String>> {
    let fields: Vec<String> = if line.contains('\t') {
        line.split('\t').map(|field| field.trim().to_owned()).collect()
    } else {
        line.split("  ")
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .map(str::to_owned)
            .collect()
    };
    (!fields.is_empty() && fields.first().is_some_and(|field| !field.is_empty())).then_some(fields)
}

fn package_from_fields(
    fields: Vec<String>,
    installed: Option<&BTreeMap<String, String>>,
) -> Result<Package, ProviderError> {
    let provider_id = fields.first().cloned().unwrap_or_default();
    if provider_id.is_empty() {
        return Err(ProviderError::Parse {
            package_source: PackageSource::Flatpak,
            operation: "parse Flatpak metadata".into(),
            technical: format!("missing application ID in fields: {fields:?}"),
        });
    }
    let name = fields
        .get(1)
        .cloned()
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| provider_id.clone());
    let summary = fields.get(2).cloned().filter(|value| !value.is_empty());
    let version = fields.get(3).cloned().filter(|value| !value.is_empty());
    let branch = fields.get(4).cloned().filter(|value| !value.is_empty());
    let origin = fields.get(5).cloned().filter(|value| !value.is_empty());
    let architecture = fields.get(6).cloned().filter(|value| !value.is_empty());
    let homepage = fields.get(7).cloned().filter(|value| !value.is_empty());
    let size_bytes = fields.get(8).and_then(|value| value.parse().ok());
    let installed_state = installed.map(|packages| packages.contains_key(&provider_id));
    let mut metadata = BTreeMap::new();
    if let Some(branch) = branch {
        metadata.insert("branch".into(), branch);
    }
    Ok(Package {
        source: PackageSource::Flatpak,
        provider_id,
        name,
        version,
        summary: summary.clone(),
        description: summary.clone(),
        installed: installed_state,
        kind: Some(PackageKind::Application),
        origin,
        architecture,
        homepage,
        license: None,
        size_bytes,
        metadata,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_tabular_flatpak_columns() {
        let fields =
            parse_columns("org.example.App\tExample App\tA useful app\t1.2\tstable\tflathub")
                .expect("fields");
        let package = package_from_fields(fields, None).expect("package");
        assert_eq!(package.provider_id, "org.example.App");
        assert_eq!(package.origin.as_deref(), Some("flathub"));
        assert_eq!(package.kind, Some(PackageKind::Application));
    }
}
