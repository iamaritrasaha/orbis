//! Universal execution stages and operation event model.
//!
//! This module provides the vocabulary for live operation progress. Both the TUI
//! and plain CLI share the same stage definitions so that an operation's lifecycle
//! is presented consistently regardless of output mode.

use serde::Serialize;

use crate::models::PackageSource;

/// Universal Orbis execution stages.
///
/// Every mutating operation progresses through a subset of these stages.
/// The currently active stage is always exactly one value.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStage {
    /// Building the provider plan (read-only).
    Planning,
    /// The plan has been presented and Orbis is waiting for user confirmation.
    AwaitingConfirmation,
    /// Requesting administrator authorization via sudo.
    Authenticating,
    /// The provider mutation command is running.
    Executing,
    /// Post-execution verification of package state.
    Verifying,
    /// Writing the durable transaction/maintenance record.
    RecordingHistory,
    /// The operation finished successfully.
    Completed,
    /// The operation finished with a failure.
    Failed,
}

impl ExecutionStage {
    /// Human-readable label for this stage.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Planning => "Planning",
            Self::AwaitingConfirmation => "Awaiting confirmation",
            Self::Authenticating => "Authenticating",
            Self::Executing => "Executing",
            Self::Verifying => "Verifying",
            Self::RecordingHistory => "Recording history",
            Self::Completed => "Completed",
            Self::Failed => "Failed",
        }
    }

    /// Whether this stage represents a terminal state.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }

    /// The ordered list of stages for a standard single-package transaction.
    pub fn transaction_stages() -> &'static [ExecutionStage] {
        &[
            Self::Planning,
            Self::AwaitingConfirmation,
            Self::Authenticating,
            Self::Executing,
            Self::Verifying,
            Self::RecordingHistory,
        ]
    }

    /// The ordered list of stages for a maintenance operation.
    pub fn maintenance_stages() -> &'static [ExecutionStage] {
        &[
            Self::Planning,
            Self::AwaitingConfirmation,
            Self::Authenticating,
            Self::Executing,
            Self::Verifying,
            Self::RecordingHistory,
        ]
    }
}

/// The stream a provider output line originated from.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OutputStream {
    /// Standard output.
    Stdout,
    /// Standard error.
    Stderr,
}

/// A single line of provider output captured during execution.
#[derive(Clone, Debug, Serialize)]
pub struct OutputLine {
    /// Which stream produced this line.
    pub stream: OutputStream,
    /// The text content (without trailing newline).
    pub content: String,
}

/// Events emitted during an Orbis operation's lifecycle.
#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationEvent {
    /// The operation has moved to a new stage.
    StageChanged {
        /// The new active stage.
        stage: ExecutionStage,
    },
    /// A line of output was received from the provider process.
    ProviderOutput(OutputLine),
    /// A non-fatal warning was produced.
    Warning {
        /// Warning message.
        message: String,
    },
    /// The operation has reached a terminal state.
    Finished {
        /// The terminal stage (Completed or Failed).
        stage: ExecutionStage,
        /// Human-readable outcome description.
        message: Option<String>,
        /// The operation ID for history reference.
        operation_id: String,
    },
}

/// An observer that receives live operation events.
///
/// Implementations must be thread-safe because events may arrive from
/// background worker threads.
pub trait ProgressObserver: Send + Sync {
    /// Called for each event during an operation's lifecycle.
    fn on_event(&self, event: &OperationEvent);
}

/// A no-op observer that discards all events.
///
/// Used in `--json` mode and in tests where progress display is not needed.
pub struct SilentObserver;

impl ProgressObserver for SilentObserver {
    fn on_event(&self, _event: &OperationEvent) {}
}

/// An observer backed by an [`std::sync::mpsc::Sender`] for channel-based event delivery.
///
/// This is the primary mechanism for delivering events to the TUI event loop.
pub struct ChannelObserver {
    sender: std::sync::mpsc::Sender<OperationEvent>,
}

impl ChannelObserver {
    /// Creates a new channel observer.
    pub fn new(sender: std::sync::mpsc::Sender<OperationEvent>) -> Self {
        Self { sender }
    }
}

impl ProgressObserver for ChannelObserver {
    fn on_event(&self, event: &OperationEvent) {
        let _ = self.sender.send(event.clone());
    }
}

/// Action title for display during progress.
#[derive(Clone, Debug)]
pub struct OperationHeader {
    /// The action being performed.
    pub title: String,
    /// The target package or maintenance scope.
    pub target: String,
    /// The source/provider.
    pub source: PackageSource,
    /// Human-readable scope label.
    pub scope: String,
    /// Whether administrator privilege is required.
    pub privileged: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct CollectingObserver {
        events: Mutex<Vec<OperationEvent>>,
    }

    impl CollectingObserver {
        fn new() -> Self {
            Self { events: Mutex::new(Vec::new()) }
        }

        fn events(&self) -> Vec<OperationEvent> {
            self.events.lock().expect("lock").clone()
        }
    }

    impl ProgressObserver for CollectingObserver {
        fn on_event(&self, event: &OperationEvent) {
            self.events.lock().expect("lock").push(event.clone());
        }
    }

    #[test]
    fn stage_labels_are_human_readable() {
        assert_eq!(ExecutionStage::Planning.label(), "Planning");
        assert_eq!(ExecutionStage::Executing.label(), "Executing");
        assert_eq!(ExecutionStage::Completed.label(), "Completed");
        assert_eq!(ExecutionStage::Failed.label(), "Failed");
    }

    #[test]
    fn terminal_stages_are_identified() {
        assert!(!ExecutionStage::Planning.is_terminal());
        assert!(!ExecutionStage::Executing.is_terminal());
        assert!(ExecutionStage::Completed.is_terminal());
        assert!(ExecutionStage::Failed.is_terminal());
    }

    #[test]
    fn transaction_stages_are_ordered() {
        let stages = ExecutionStage::transaction_stages();
        assert!(stages.len() >= 4);
        assert_eq!(stages[0], ExecutionStage::Planning);
        assert!(stages.contains(&ExecutionStage::Executing));
        assert!(stages.contains(&ExecutionStage::Verifying));
    }

    #[test]
    fn silent_observer_accepts_events_without_effect() {
        let observer = SilentObserver;
        observer.on_event(&OperationEvent::StageChanged { stage: ExecutionStage::Planning });
        observer.on_event(&OperationEvent::Finished {
            stage: ExecutionStage::Completed,
            message: None,
            operation_id: "test".into(),
        });
    }

    #[test]
    fn collecting_observer_captures_all_events() {
        let observer = Arc::new(CollectingObserver::new());
        observer.on_event(&OperationEvent::StageChanged { stage: ExecutionStage::Planning });
        observer.on_event(&OperationEvent::ProviderOutput(OutputLine {
            stream: OutputStream::Stdout,
            content: "test output".into(),
        }));
        observer.on_event(&OperationEvent::Finished {
            stage: ExecutionStage::Completed,
            message: Some("done".into()),
            operation_id: "tx-123".into(),
        });
        assert_eq!(observer.events().len(), 3);
    }

    #[test]
    fn channel_observer_delivers_events() {
        let (tx, rx) = std::sync::mpsc::channel();
        let observer = ChannelObserver::new(tx);
        observer.on_event(&OperationEvent::StageChanged { stage: ExecutionStage::Executing });
        let event = rx.try_recv().expect("event delivered");
        assert!(matches!(event, OperationEvent::StageChanged { stage: ExecutionStage::Executing }));
    }

    #[test]
    fn events_serialize_to_json_without_ansi() {
        let event = OperationEvent::StageChanged { stage: ExecutionStage::Executing };
        let json = serde_json::to_string(&event).expect("serializes");
        assert!(json.contains("executing"));
        assert!(!json.contains("\x1b"));
    }

    #[test]
    fn output_line_preserves_stream_identity() {
        let stdout_line =
            OutputLine { stream: OutputStream::Stdout, content: "stdout content".into() };
        let stderr_line =
            OutputLine { stream: OutputStream::Stderr, content: "stderr content".into() };
        assert_eq!(stdout_line.stream, OutputStream::Stdout);
        assert_eq!(stderr_line.stream, OutputStream::Stderr);
    }
}
