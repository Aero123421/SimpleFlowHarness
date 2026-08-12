//! What sfh is allowed to conclude from a structured tool protocol.
//!
//! Every preset tool is asked for machine-readable output, and every one of
//! them can fail in a way its exit code does not describe: a usage error on
//! stdout, a truncated event stream, an envelope that never carries the
//! terminal record. Before v1.2 a parser that could not make sense of the
//! stream fell back to "the raw stdout is the answer", which turned a broken
//! invocation into a plausible-looking success and put that text into the next
//! step's prompt.
//!
//! This module holds the evidence a parser must produce so the execution layer
//! can decide fail-closed instead of fail-open. sfh judges only mechanical
//! facts here - did the documented terminal record arrive, was every record
//! well-formed - never whether the model's answer was any good.

/// Whether the structured stream held together end to end.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProtocolState {
    /// Not a structured protocol at all: a custom `cmd:` step, whose stdout is
    /// the contract the flow author chose. Unchanged from every prior release.
    #[default]
    Plain,
    /// Parsed, and the documented terminal record was present.
    Valid,
    /// Records parsed, but the run never reached its documented terminal record.
    MissingTerminal,
    /// The stream itself was not the documented shape (unparseable, wrong
    /// envelope, oversized record, non-JSON where JSON was required).
    Invalid,
}

impl ProtocolState {
    pub fn as_str(self) -> &'static str {
        match self {
            ProtocolState::Plain => "plain",
            ProtocolState::Valid => "valid",
            ProtocolState::MissingTerminal => "missing_terminal",
            ProtocolState::Invalid => "invalid",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "plain" => Some(Self::Plain),
            "valid" => Some(Self::Valid),
            "missing_terminal" => Some(Self::MissingTerminal),
            "invalid" => Some(Self::Invalid),
            _ => None,
        }
    }
}

/// What the parser observed, kept separately from the answer text so the
/// execution layer never has to re-derive it from the text itself.
#[derive(Clone, Debug, Default)]
pub struct ProtocolEvidence {
    pub protocol: ProtocolState,
    /// The documented terminal record for this adapter arrived.
    pub terminal_seen: bool,
    /// Present only when the terminal record itself reports success/failure.
    /// `None` means the adapter's terminal record carries no verdict, which is
    /// not the same as a reported failure.
    pub terminal_success: Option<bool>,
    /// A final assistant message was present in the stream (or its side file).
    pub final_message_seen: bool,
    /// Records the adapter could not parse. Non-zero is `Invalid` for adapters
    /// whose documented output is pure JSON/JSONL.
    pub malformed_records: u32,
    /// Bounded, sfh-authored explanation for `runs why` and `step_end`.
    pub diagnostic: Option<String>,
}

impl ProtocolEvidence {
    /// A custom `cmd:` step: no structured contract to hold anyone to.
    pub fn plain() -> Self {
        ProtocolEvidence {
            protocol: ProtocolState::Plain,
            ..Default::default()
        }
    }

    /// The stream was not the documented shape at all.
    pub fn invalid(diagnostic: impl Into<String>) -> Self {
        ProtocolEvidence {
            protocol: ProtocolState::Invalid,
            diagnostic: Some(diagnostic.into()),
            ..Default::default()
        }
    }

    /// True when this evidence permits treating the step as successful, given
    /// the process itself exited acceptably. A step that needs a final message
    /// (`allow_empty: false`) additionally requires `final_message_seen`, which
    /// the caller checks against the chain text it actually got.
    pub fn allows_success(&self) -> bool {
        match self.protocol {
            ProtocolState::Plain | ProtocolState::Valid => self.terminal_success != Some(false),
            ProtocolState::MissingTerminal | ProtocolState::Invalid => false,
        }
    }

    /// Only a positively identified terminal success record may excuse a
    /// non-zero exit status. Raw text, an unknown status, a malformed envelope
    /// or a missing terminal record must never be corrected to exit 0
    /// (spec P0-01): a tool that printed its usage message and exited 1 has
    /// non-empty stdout and no in-band failure flag, which is exactly the shape
    /// the old correction accepted.
    pub fn certifies_success(&self) -> bool {
        self.protocol == ProtocolState::Valid
            && self.terminal_seen
            && self.terminal_success == Some(true)
    }

    /// Human/machine reason this protocol cannot be called complete.
    pub fn failure_reason(&self, tool: &str) -> Option<String> {
        if let Some(d) = &self.diagnostic {
            if !self.allows_success() {
                return Some(d.clone());
            }
        }
        match self.protocol {
            ProtocolState::Plain | ProtocolState::Valid => {
                if self.terminal_success == Some(false) {
                    Some(format!(
                        "{tool} reported an in-band failure in its terminal record"
                    ))
                } else {
                    None
                }
            }
            ProtocolState::MissingTerminal => Some(format!(
                "{tool} structured output ended without its documented terminal record, so sfh cannot tell whether the turn finished"
            )),
            ProtocolState::Invalid => Some(format!(
                "{tool} structured output did not match its documented machine-readable format"
            )),
        }
    }
}

/// The name a flow author sees for the protocol a step expects.
pub fn expected_kind(parse: &crate::preset::OutputParse) -> &'static str {
    use crate::preset::OutputParse::*;
    match parse {
        Stdout => "stdout",
        CodexJsonl(_) => "codex-jsonl",
        ClaudeJson => "claude-json",
        OpencodeNdjson => "opencode-ndjson",
        GrokJson => "grok-json",
        AgyJson => "agy-json",
        PiJsonl => "pi-jsonl",
        CursorJson => "cursor-json",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_positive_terminal_record_can_excuse_a_nonzero_exit() {
        // The exact P0-01 shape: usable-looking text, nothing said it failed,
        // but no adapter ever confirmed a terminal success record.
        let raw_text_fallback = ProtocolEvidence {
            protocol: ProtocolState::Invalid,
            ..Default::default()
        };
        assert!(!raw_text_fallback.certifies_success());
        let unknown_status = ProtocolEvidence {
            protocol: ProtocolState::Valid,
            terminal_seen: true,
            terminal_success: None,
            ..Default::default()
        };
        assert!(!unknown_status.certifies_success());
        let missing_terminal = ProtocolEvidence {
            protocol: ProtocolState::MissingTerminal,
            ..Default::default()
        };
        assert!(!missing_terminal.certifies_success());
        let real_success = ProtocolEvidence {
            protocol: ProtocolState::Valid,
            terminal_seen: true,
            terminal_success: Some(true),
            ..Default::default()
        };
        assert!(real_success.certifies_success());
    }

    #[test]
    fn incomplete_protocols_never_allow_success() {
        for state in [ProtocolState::MissingTerminal, ProtocolState::Invalid] {
            let e = ProtocolEvidence {
                protocol: state,
                terminal_success: Some(true),
                ..Default::default()
            };
            assert!(!e.allows_success(), "{state:?} must not allow success");
            assert!(e.failure_reason("tool").is_some());
        }
        assert!(ProtocolEvidence::plain().allows_success());
    }
}
