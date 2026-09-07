//! Deterministic, evidence-aware package explanations.

use serde::Serialize;

use crate::models::{Package, PackageKind, PackageSource};

/// The provenance of a sentence in an Orbis Brief.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    /// Directly supplied by the package provider.
    ProviderMetadata,
    /// A conservative Orbis rule or small maintained knowledge entry.
    OrbisInterpretation,
}

/// A concise provenance note.
#[derive(Clone, Debug, Serialize)]
pub struct Evidence {
    /// Provenance category.
    pub kind: EvidenceKind,
    /// What the evidence supports.
    pub detail: String,
}

/// A deterministic, human-oriented package explanation.
#[derive(Clone, Debug, Serialize)]
pub struct PackageBrief {
    /// Package the brief describes.
    pub package: Package,
    /// One-line plain-English lead.
    pub headline: String,
    /// Short explanatory paragraphs.
    pub paragraphs: Vec<String>,
    /// Whether users normally run it directly, when confidently known.
    pub normally_run_directly: Option<bool>,
    /// Example uses from a maintained knowledge entry, when available.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub examples: Vec<String>,
    /// Context that matters before a future removal operation.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub caution: Option<String>,
    /// Overall explanation confidence.
    pub confidence: String,
    /// Evidence provenance.
    pub evidence: Vec<Evidence>,
}

/// Builds an offline Orbis Brief without calling an external service.
pub fn build_brief(package: Package) -> PackageBrief {
    let key = package.provider_id.to_ascii_lowercase();
    let mut evidence = Vec::new();
    let mut examples = Vec::new();
    let (headline, paragraphs, direct, confidence) = if key == "btop" {
        evidence.push(Evidence {
            kind: EvidenceKind::OrbisInterpretation,
            detail: "Maintained entry for btop, checked against provider metadata.".into(),
        });
        examples.extend(
            [
                "watch CPU and memory usage",
                "inspect disks, network activity, temperatures, and processes",
            ]
            .map(str::to_owned),
        );
        (
            "A terminal system monitor.".into(),
            vec![
                "It lets you watch CPU usage, memory, disks, network activity, temperatures, and running processes in real time.".into(),
                "Think of it as a more visual and interactive alternative to the traditional `top` command.".into(),
            ],
            Some(true),
            "high",
        )
    } else if key == "ffmpeg" {
        evidence.push(Evidence {
            kind: EvidenceKind::OrbisInterpretation,
            detail: "Maintained entry for ffmpeg, checked against provider metadata.".into(),
        });
        examples.extend(
            [
                "convert media formats",
                "compress or resize video",
                "extract audio",
                "automate media processing",
            ]
            .map(str::to_owned),
        );
        (
            "A powerful toolkit for working with video and audio.".into(),
            vec!["It can convert media formats, compress files, extract audio, resize videos, and automate media processing.".into()],
            Some(true),
            "high",
        )
    } else if key == "libssl-dev" || key.ends_with("/libssl-dev") {
        evidence.push(Evidence {
            kind: EvidenceKind::OrbisInterpretation,
            detail: "Maintained entry for the OpenSSL development package.".into(),
        });
        (
            "Development files for OpenSSL.".into(),
            vec!["You normally do not use this package directly. Developers and build tools need it when compiling software that uses encrypted networking, HTTPS, certificates, or other OpenSSL functionality.".into()],
            Some(false),
            "high",
        )
    } else if matches!(package.source, PackageSource::Cargo) {
        evidence.push(Evidence {
            kind: EvidenceKind::ProviderMetadata,
            detail: "Cargo registry metadata and Cargo's installed-tool interface.".into(),
        });
        let summary =
            package.summary.clone().unwrap_or_else(|| "Rust CLI tool managed by Cargo.".into());
        (
            summary.clone(),
            vec![summary, "Installed through Cargo as a user-level Rust CLI application.".into()],
            Some(true),
            "high",
        )
    } else if matches!(package.source, PackageSource::Npm) {
        evidence.push(Evidence {
            kind: EvidenceKind::ProviderMetadata,
            detail: "npm registry metadata and global package listing.".into(),
        });
        let summary = package.summary.clone().unwrap_or_else(|| "Global npm package.".into());
        (
            summary.clone(),
            vec![summary, "This copy is managed as a globally installed npm package.".into()],
            Some(true),
            "high",
        )
    } else if matches!(package.source, PackageSource::Pnpm) {
        evidence.push(Evidence {
            kind: EvidenceKind::ProviderMetadata,
            detail: "pnpm registry metadata and global package listing.".into(),
        });
        let summary = package.summary.clone().unwrap_or_else(|| "Global pnpm package.".into());
        (
            summary.clone(),
            vec![summary, "This copy is managed as a globally installed pnpm package.".into()],
            Some(true),
            "high",
        )
    } else if matches!(package.source, PackageSource::Uv) {
        evidence.push(Evidence {
            kind: EvidenceKind::ProviderMetadata,
            detail: "uv tool installed-state metadata; registry purpose metadata was not queried."
                .into(),
        });
        ("A Python command-line tool managed by uv.".into(), vec!["It is installed in an isolated persistent uv tool environment.".into(), "Orbis does not currently have enough registry metadata to provide a richer purpose description.".into()], Some(true), "medium")
    } else if matches!(package.source, PackageSource::Pipx) {
        evidence.push(Evidence {
            kind: EvidenceKind::ProviderMetadata,
            detail: "pipx structured installed-package snapshot; registry purpose metadata was not queried.".into(),
        });
        ("A Python command-line tool managed by pipx.".into(), vec!["It is installed in an isolated pipx environment.".into(), "Orbis does not currently have enough registry metadata to provide a richer purpose description.".into()], Some(true), "medium")
    } else {
        evidence.push(Evidence { kind: EvidenceKind::ProviderMetadata, detail: "The wording below is taken from provider metadata; Orbis has not invented a richer description.".into() });
        let headline = package.summary.clone().unwrap_or_else(|| {
            "Package metadata is available, but no summary was supplied.".into()
        });
        let mut paragraphs = Vec::new();
        if let Some(description) = package.description.clone().or_else(|| package.summary.clone()) {
            paragraphs.push(description);
        }
        let direct = match package.kind {
            Some(PackageKind::Library | PackageKind::DevelopmentFiles | PackageKind::Runtime) => {
                Some(false)
            }
            Some(PackageKind::Application | PackageKind::CliTool) => Some(true),
            Some(PackageKind::Service | PackageKind::Unknown) | None => None,
        };
        (headline, paragraphs, direct, "medium")
    };

    let kind = package.kind;
    let caution = match kind {
        Some(PackageKind::Library | PackageKind::DevelopmentFiles | PackageKind::Runtime) => Some("Other software may depend on this component. Before a future removal, inspect dependants rather than assuming it is unused.".into()),
        _ => None,
    };
    let inferred_kind = kind.map(|kind| {
        format!(
            "Orbis classified this as {} from package metadata and conservative naming rules.",
            kind.label()
        )
    });
    if let Some(detail) = inferred_kind {
        evidence.push(Evidence { kind: EvidenceKind::OrbisInterpretation, detail });
    }
    PackageBrief {
        package,
        headline,
        paragraphs,
        normally_run_directly: direct,
        examples,
        caution,
        confidence: confidence.into(),
        evidence,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::models::{Package, PackageSource};

    fn package(id: &str, kind: Option<PackageKind>) -> Package {
        Package {
            source: PackageSource::Apt,
            provider_id: id.into(),
            name: id.into(),
            version: None,
            summary: Some("summary".into()),
            description: Some("description".into()),
            installed: None,
            kind,
            origin: None,
            architecture: None,
            homepage: None,
            license: None,
            size_bytes: None,
            metadata: Default::default(),
        }
    }

    #[test]
    fn known_entries_are_specific_and_evidence_aware() {
        let brief = build_brief(package("btop", Some(PackageKind::CliTool)));
        assert_eq!(brief.headline, "A terminal system monitor.");
        assert!(
            brief
                .evidence
                .iter()
                .any(|item| matches!(item.kind, EvidenceKind::OrbisInterpretation))
        );
    }

    #[test]
    fn unknown_package_does_not_get_fabricated_explanation() {
        let brief = build_brief(package("unknown", None));
        assert_eq!(brief.confidence, "medium");
        assert!(
            brief.evidence.iter().any(|item| matches!(item.kind, EvidenceKind::ProviderMetadata))
        );
    }
}
