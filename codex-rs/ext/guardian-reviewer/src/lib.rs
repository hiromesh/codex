//! Minimal Guardian evidence and context helpers retained for multi-agent
//! authorization. Automatic review policy and session machinery are removed.

mod context_mode;
mod review_evidence;
mod sender_evidence;

pub use context_mode::GuardianContextMode;
pub use review_evidence::GuardianAuthorizationVersion;
pub use review_evidence::GuardianReviewEvidence;
pub use review_evidence::GuardianReviewEvidenceRecord;
pub use review_evidence::GuardianRootSnapshot;
pub use review_evidence::GuardianUserInputSnapshot;
pub use sender_evidence::GuardianSenderExchange;
pub use sender_evidence::guardian_sender_exchanges;
