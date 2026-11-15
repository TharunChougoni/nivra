//! Exact single-instrument Upstox protection dispatch using original archive bytes.
//! Kotak rederivation remains unported and refuses; there is no provider failover.
use crate::{StoreError, archive::LegacyUpstoxCapture};
use sadr_core::{
    market::{AccountSource, ArchivedQuoteReceipt, Calendar},
    paper_source::{PaperSourceBinding, Source},
};
pub fn quote_receipt(
    capture: &LegacyUpstoxCapture,
    binding: &PaperSourceBinding,
    identity: &AccountSource,
    calendar: &Calendar,
) -> Result<ArchivedQuoteReceipt, StoreError> {
    if binding.source() != Source::Upstox {
        return Err(StoreError::Quote);
    }
    capture.derive_receipt(
        std::slice::from_ref(binding.instrument()),
        identity,
        calendar,
    )
}
