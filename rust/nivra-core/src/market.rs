//! Immutable domain contracts and receipt/freshness checks, without provider I/O.
//!
//! Structural validity is not tradability or authenticated provenance. A future
//! trusted archive adapter must rederive the snapshot from exact original bytes.

use crate::{
    finance::{CashOrderGrid, FinancialError, Price},
    time::{ExchangeDate, LocalTime, Timestamp},
};
use serde::{Deserialize, Deserializer, Serialize, de};
use std::{
    collections::{BTreeMap, BTreeSet},
    error::Error,
    fmt,
    time::Duration,
};

/// Single source for the hard financial quote/receipt age limit.
pub const MAX_FINANCIAL_AGE: Duration = Duration::from_secs(2);
pub const MAX_FINANCIAL_AGE_MS: i64 = MAX_FINANCIAL_AGE.as_millis() as i64;
pub const MAX_FINANCIAL_AGE_MICROS: i64 = MAX_FINANCIAL_AGE.as_micros() as i64;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MarketError {
    InvalidInstrument,
    InvalidBar,
    InvalidQuote,
    InvalidSnapshot,
    InvalidContinuousSession,
    InvalidCalendar,
    CalendarCoverageMissing,
    InstrumentMismatch,
    InvalidSourceIdentity,
    InvalidReceipt,
    ReceiptMismatch,
    FutureOrStaleReceipt,
    FutureOrStaleQuote,
}

impl fmt::Display for MarketError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::InvalidInstrument => "invalid_instrument_contract",
            Self::InvalidBar => "invalid_bar_contract",
            Self::InvalidQuote => "invalid_quote_contract",
            Self::InvalidSnapshot => "invalid_snapshot_contract",
            Self::InvalidContinuousSession => "invalid_continuous_session",
            Self::InvalidCalendar => "invalid_calendar_contract",
            Self::CalendarCoverageMissing => "calendar_coverage_missing",
            Self::InstrumentMismatch => "instrument_identity_mismatch",
            Self::InvalidSourceIdentity => "invalid_source_identity",
            Self::InvalidReceipt => "invalid_archive_receipt",
            Self::ReceiptMismatch => "archive_receipt_mismatch",
            Self::FutureOrStaleReceipt => "future_or_stale_archive_receipt",
            Self::FutureOrStaleQuote => "future_or_stale_quote",
        })
    }
}

impl Error for MarketError {}

macro_rules! validated_record {
    ($type:ident, $fields:ident) => {
        impl<'de> Deserialize<'de> for $type {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                Self::try_from($fields::deserialize(deserializer)?).map_err(de::Error::custom)
            }
        }
    };
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BarFields {
    instrument_key: String,
    timestamp: Timestamp,
    available_at: Timestamp,
    open: Price,
    high: Price,
    low: Price,
    close: Price,
    volume: u64,
}

/// Interval-independent OHLC evidence. Validity does not certify an instrument,
/// calendar, completed interval, source, or executable bid/ask liquidity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Bar(BarFields);
impl TryFrom<BarFields> for Bar {
    type Error = MarketError;
    fn try_from(value: BarFields) -> Result<Self, MarketError> {
        if value.low > value.open.min(value.close)
            || value.high < value.open.max(value.close)
            || value.available_at <= value.timestamp
        {
            return Err(MarketError::InvalidBar);
        }
        Ok(Self(value))
    }
}
validated_record!(Bar, BarFields);
impl Bar {
    pub fn instrument_key(&self) -> &str {
        &self.0.instrument_key
    }
    pub fn timestamp(&self) -> Timestamp {
        self.0.timestamp
    }
    pub fn available_at(&self) -> Timestamp {
        self.0.available_at
    }
    pub fn ohlc(&self) -> [Price; 4] {
        [self.0.open, self.0.high, self.0.low, self.0.close]
    }
    pub fn volume(&self) -> u64 {
        self.0.volume
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum InstrumentKind {
    Index,
    Cash,
    Call,
    Put,
    Future,
    Etf,
}

impl InstrumentKind {
    pub fn is_derivative(&self) -> bool {
        matches!(self, Self::Future | Self::Call | Self::Put)
    }

    pub fn display_only(&self) -> bool {
        // Display metadata only. Execution admission is `PaperMandate`, not
        // this flag; metadata support alone does not authorize a fill.
        !matches!(self, Self::Cash)
    }
}

/// Exchange segment carried by an instrument key prefix.
///
/// Phase 0 admits only `NseEq` execution; the other segments exist so later
/// index feeds and F&O contracts do not grow a second instrument model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum Segment {
    NseEq,
    NseIndex,
    NseFo,
}

impl Segment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NseEq => "NSE_EQ",
            Self::NseIndex => "NSE_INDEX",
            Self::NseFo => "NSE_FO",
        }
    }

    /// Segment named by the key prefix before `|`, if it is a known segment.
    pub fn from_key(key: &str) -> Option<Self> {
        match key.split_once('|')?.0 {
            "NSE_EQ" => Some(Self::NseEq),
            "NSE_INDEX" => Some(Self::NseIndex),
            "NSE_FO" => Some(Self::NseFo),
            _ => None,
        }
    }
}

/// Contract terms attached to an instrument identity.
///
/// Cash uses lot 1 and multiplier 1; derivatives carry the real lot, tick,
/// expiry, strike, option type and (later) freeze quantity. This is a view of
/// the single `Instrument` record, not a parallel instrument model.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ContractSpec {
    pub lot_size: u64,
    pub qty_multiplier: u64,
    pub tick: Price,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<ExchangeDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strike: Option<Price>,
    /// `Call` or `Put` only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub option: Option<InstrumentKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze_quantity: Option<u64>,
}

/// Named exchange session calendar. Phase 0 wires only the NSE cash session;
/// CDS and MCX exist as names with documented hours and are not activated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionCalendarId {
    /// NSE cash equity and index data, 09:15-15:30 IST.
    NseCash,
    /// NSE F&O, 09:15-15:30 IST today; named separately because rules diverge.
    NseFo,
    /// Currency derivatives, about 09:00-17:00 IST (not activated).
    Cds,
    /// MCX non-agri, 09:00-23:55 IST through 2027-03-12 (not activated).
    McxNonAgri,
}

impl SessionCalendarId {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::NseCash => "nse_cash",
            Self::NseFo => "nse_fo",
            Self::Cds => "cds",
            Self::McxNonAgri => "mcx_non_agri",
        }
    }

    /// Documented continuous hours. A named calendar is not an activation
    /// grant; holiday coverage still comes from the reviewed `Calendar` record.
    pub fn hours(self) -> (LocalTime, LocalTime) {
        let (opening, closing) = match self {
            Self::NseCash | Self::NseFo => ("09:15:00", "15:30:00"),
            Self::Cds => ("09:00:00", "17:00:00"),
            Self::McxNonAgri => ("09:00:00", "23:55:00"),
        };
        (
            LocalTime::parse(opening).expect("documented session open"),
            LocalTime::parse(closing).expect("documented session close"),
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum SecurityType {
    Normal,
    Sme,
    Relist,
    Pca,
    Ipo,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InstrumentFields {
    key: String,
    symbol: String,
    kind: InstrumentKind,
    lot_size: u64,
    tick_size: Price,
    version: String,
    #[serde(default)]
    expiry: Option<ExchangeDate>,
    #[serde(default)]
    strike: Option<Price>,
    #[serde(default)]
    underlying: Option<String>,
    #[serde(default)]
    synthetic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cas_eligible: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    security_type: Option<SecurityType>,
    /// Absent historic rows infer the segment from the key prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    segment: Option<Segment>,
    #[serde(
        default = "default_qty_multiplier",
        skip_serializing_if = "is_default_qty_multiplier"
    )]
    qty_multiplier: u64,
}

fn default_qty_multiplier() -> u64 {
    1
}

fn is_default_qty_multiplier(value: &u64) -> bool {
    *value == 1
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Instrument(InstrumentFields);

impl TryFrom<InstrumentFields> for Instrument {
    type Error = MarketError;
    fn try_from(value: InstrumentFields) -> Result<Self, MarketError> {
        if value.lot_size == 0
            || value.qty_multiplier == 0
            || value.version.is_empty()
            || (!matches!(value.kind, InstrumentKind::Cash | InstrumentKind::Etf)
                && (value.cas_eligible.is_some() || value.security_type.is_some()))
            || (matches!(value.kind, InstrumentKind::Call | InstrumentKind::Put)
                && (value.expiry.is_none()
                    || value.strike.is_none()
                    || value.underlying.as_ref().is_none_or(String::is_empty)))
            || (value.kind == InstrumentKind::Future
                && (value.expiry.is_none()
                    || value.strike.is_some()
                    || value.underlying.as_ref().is_none_or(String::is_empty)))
            || (value.key.starts_with("NSE_INDEX|") && value.kind != InstrumentKind::Index)
            || value.segment.is_some_and(|segment| {
                Segment::from_key(&value.key).is_some_and(|key_segment| key_segment != segment)
            })
            || (value.synthetic && !value.key.starts_with("TEST|"))
        {
            return Err(MarketError::InvalidInstrument);
        }
        Ok(Self(value))
    }
}
validated_record!(Instrument, InstrumentFields);

impl Instrument {
    pub fn symbol(&self) -> &str {
        &self.0.symbol
    }
    pub fn key(&self) -> &str {
        &self.0.key
    }
    pub fn version(&self) -> &str {
        &self.0.version
    }
    pub fn kind(&self) -> InstrumentKind {
        self.0.kind
    }
    pub fn lot_size(&self) -> u64 {
        self.0.lot_size
    }
    pub fn tick_size(&self) -> Price {
        self.0.tick_size
    }
    /// Explicit segment, else the segment named by the key prefix.
    pub fn segment(&self) -> Option<Segment> {
        self.0.segment.or_else(|| Segment::from_key(&self.0.key))
    }
    pub fn qty_multiplier(&self) -> u64 {
        self.0.qty_multiplier
    }
    /// Single-record view of the contract terms; no second instrument struct.
    pub fn contract_spec(&self) -> ContractSpec {
        ContractSpec {
            lot_size: self.0.lot_size,
            qty_multiplier: self.0.qty_multiplier,
            tick: self.0.tick_size,
            expiry: self.0.expiry,
            strike: self.0.strike,
            option: matches!(self.0.kind, InstrumentKind::Call | InstrumentKind::Put)
                .then_some(self.0.kind),
            freeze_quantity: None,
        }
    }
    pub fn synthetic(&self) -> bool {
        self.0.synthetic
    }
    pub fn expiry(&self) -> Option<ExchangeDate> {
        self.0.expiry
    }
    pub fn strike(&self) -> Option<Price> {
        self.0.strike
    }
    pub fn underlying(&self) -> Option<&str> {
        self.0.underlying.as_deref()
    }
    pub fn is_derivative(&self) -> bool {
        self.kind().is_derivative()
    }
    pub fn validate_order(&self, units: i64, price: Price) -> Result<(), FinancialError> {
        if self.kind() == InstrumentKind::Index {
            return Err(FinancialError::InvalidOrderGrid);
        }
        CashOrderGrid::new(self.lot_size(), self.tick_size())?.validate(units, price)
    }
    /// Single admission gate for the paper book; see [`PaperMandate`].
    pub fn validate_paper_execution(&self) -> Result<(), FinancialError> {
        PaperMandate::CashEquity.admit(self)
    }
}

/// Execution mandate for the paper book. Admission lives here so runtime,
/// archive and store callers share one gate instead of repeating `kind != Cash`
/// checks. Cash equity is lot 1; the option mandate is buy-only long premium.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PaperMandate {
    CashEquity,
    /// Phase 2: buy-only Nifty weekly options, long premium only.
    NiftyOptionLong,
}

/// Configured option mandate identity. The underlying must match the contract
/// payload; a symbol alone is never enough to authorize a derivative.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NiftyOptionMandate<'a> {
    pub underlying_key: &'a str,
    pub underlying_symbol: &'a str,
}

/// One option order request for mandate admission. Units are traded units
/// (already a lot multiple for a valid order); `held_units` is the same-key
/// long holding available to sell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OptionOrder<'a> {
    pub contract: &'a OptionContract,
    pub buy: bool,
    pub units: u64,
    pub held_units: u64,
    pub today: ExchangeDate,
}

impl PaperMandate {
    /// Admit or refuse one instrument for this mandate. Cash errors stay
    /// `InvalidOrderGrid` so existing activity attribution does not churn.
    pub fn admit(self, instrument: &Instrument) -> Result<(), FinancialError> {
        match self {
            Self::CashEquity => {
                if instrument.kind() != InstrumentKind::Cash || instrument.lot_size() != 1 {
                    return Err(FinancialError::InvalidOrderGrid);
                }
                Ok(())
            }
            Self::NiftyOptionLong => {
                if !matches!(
                    instrument.kind(),
                    InstrumentKind::Call | InstrumentKind::Put
                ) || instrument.segment() != Some(Segment::NseFo)
                    || instrument.lot_size() == 0
                    || instrument.qty_multiplier() != 1
                    || instrument.expiry().is_none()
                    || instrument.strike().is_none()
                    || instrument.underlying().is_none_or(str::is_empty)
                {
                    return Err(FinancialError::InvalidOptionMandate);
                }
                Ok(())
            }
        }
    }

    /// Full buy-only long-option admission: identity, underlying, lot grid,
    /// buy-to-open only, sells against held units, and no hold past expiry.
    pub fn admit_option(
        self,
        order: &OptionOrder<'_>,
        mandate: &NiftyOptionMandate<'_>,
    ) -> Result<(), FinancialError> {
        if self != Self::NiftyOptionLong {
            return Err(FinancialError::InvalidOptionMandate);
        }
        let contract = order.contract;
        if !matches!(contract.option, InstrumentKind::Call | InstrumentKind::Put)
            || Segment::from_key(&contract.key) != Some(Segment::NseFo)
            || contract.key.is_empty()
            || contract.symbol.is_empty()
            || contract.lot_size == 0
        {
            return Err(FinancialError::InvalidOptionMandate);
        }
        if contract.underlying_key != mandate.underlying_key
            || contract.underlying_symbol != mandate.underlying_symbol
        {
            return Err(FinancialError::OptionUnderlyingMismatch);
        }
        // A long option must never be held past its expiry. Opening on expiry
        // day is refused; a same-day sell is the explicit flatten path.
        if contract.expiry < order.today || (contract.expiry == order.today && order.buy) {
            return Err(FinancialError::OptionExpiry);
        }
        if order.units == 0 || !order.units.is_multiple_of(contract.lot_size) {
            return Err(FinancialError::OptionQuantityGrid);
        }
        if !order.buy && (order.held_units == 0 || order.units > order.held_units) {
            return Err(FinancialError::OptionShortOpen);
        }
        Ok(())
    }
}

/// One validated Nifty weekly option contract from the bounded universe.
///
/// `tick` is INR (the provider master stores paise; conversion happens once in
/// the universe builder). This is a contract identity, never an execution
/// authorization on its own.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OptionContract {
    pub key: String,
    pub symbol: String,
    pub underlying_key: String,
    pub underlying_symbol: String,
    pub expiry: ExchangeDate,
    pub strike: Price,
    /// `Call` or `Put` only.
    pub option: InstrumentKind,
    pub lot_size: u64,
    pub tick: Price,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub freeze_quantity: Option<u64>,
    #[serde(default)]
    pub weekly: bool,
}

impl OptionContract {
    pub fn validate(&self) -> Result<(), MarketError> {
        if self.key.is_empty()
            || self.key.len() > 256
            || self.symbol.is_empty()
            || self.symbol.len() > 256
            || self.underlying_key.is_empty()
            || self.underlying_symbol.is_empty()
            || Segment::from_key(&self.key) != Some(Segment::NseFo)
            || !matches!(self.option, InstrumentKind::Call | InstrumentKind::Put)
            || self.lot_size == 0
            || self.freeze_quantity == Some(0)
        {
            return Err(MarketError::InvalidInstrument);
        }
        Ok(())
    }

    /// Domain instrument identity for admission, quotes and the planner.
    pub fn instrument(&self) -> Result<Instrument, MarketError> {
        self.validate()?;
        Instrument::try_from(InstrumentFields {
            key: self.key.clone(),
            symbol: self.symbol.clone(),
            kind: self.option,
            lot_size: self.lot_size,
            tick_size: self.tick,
            version: format!("nse-fo-bod-{}", self.expiry),
            expiry: Some(self.expiry),
            strike: Some(self.strike),
            underlying: Some(self.underlying_symbol.clone()),
            synthetic: false,
            cas_eligible: None,
            security_type: None,
            segment: Some(Segment::NseFo),
            qty_multiplier: 1,
        })
    }
}

/// Bounded runtime option catalog for one session and one expiry. This is a
/// runtime-only view (never serialized into the cash book); it is the only
/// source of option lot, tick, expiry and underlying identity for admission.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionCatalog {
    pub session: ExchangeDate,
    pub underlying_key: String,
    pub underlying_symbol: String,
    pub expiry: ExchangeDate,
    pub spot: Price,
    pub contracts: BTreeMap<String, OptionContract>,
}

impl OptionCatalog {
    /// Maximum contracts in one bounded shortlist (~40 strikes x CE/PE).
    pub const MAX_CONTRACTS: usize = 402;

    pub fn try_new(
        session: ExchangeDate,
        underlying_key: impl Into<String>,
        underlying_symbol: impl Into<String>,
        expiry: ExchangeDate,
        spot: Price,
        contracts: impl IntoIterator<Item = OptionContract>,
    ) -> Result<Self, MarketError> {
        let underlying_key = underlying_key.into();
        let underlying_symbol = underlying_symbol.into();
        if underlying_key.is_empty()
            || underlying_symbol.is_empty()
            || Segment::from_key(&underlying_key) != Some(Segment::NseIndex)
            || expiry < session
        {
            return Err(MarketError::InvalidInstrument);
        }
        let mut map = BTreeMap::new();
        for contract in contracts {
            contract.validate()?;
            if contract.underlying_key != underlying_key
                || contract.underlying_symbol != underlying_symbol
                || contract.expiry != expiry
                || map.insert(contract.key.clone(), contract).is_some()
            {
                return Err(MarketError::InvalidInstrument);
            }
        }
        if map.is_empty() || map.len() > Self::MAX_CONTRACTS {
            return Err(MarketError::InvalidInstrument);
        }
        Ok(Self {
            session,
            underlying_key,
            underlying_symbol,
            expiry,
            spot,
            contracts: map,
        })
    }

    pub fn mandate(&self) -> NiftyOptionMandate<'_> {
        NiftyOptionMandate {
            underlying_key: &self.underlying_key,
            underlying_symbol: &self.underlying_symbol,
        }
    }

    pub fn contract(&self, key: &str) -> Option<&OptionContract> {
        self.contracts.get(key)
    }
}

/// Explicit instrument metadata reference row for display. Unknown fields are null, never guessed.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstrumentRef {
    pub key: String,
    pub exchange: String,
    pub segment: String,
    pub kind: InstrumentKind,
    pub symbol: String,
    pub underlying: Option<String>,
    pub expiry: Option<ExchangeDate>,
    pub strike: Option<Price>,
    pub lot_size: u64,
    pub tick: Price,
}

impl InstrumentRef {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        key: impl Into<String>,
        exchange: impl Into<String>,
        segment: impl Into<String>,
        kind: InstrumentKind,
        symbol: impl Into<String>,
        underlying: Option<String>,
        expiry: Option<ExchangeDate>,
        strike: Option<Price>,
        lot_size: u64,
        tick: Price,
    ) -> Result<Self, MarketError> {
        if lot_size == 0 {
            return Err(MarketError::InvalidInstrument);
        }
        Ok(Self {
            key: key.into(),
            exchange: exchange.into(),
            segment: segment.into(),
            kind,
            symbol: symbol.into(),
            underlying,
            expiry,
            strike,
            lot_size,
            tick,
        })
    }

    pub fn from_instrument(
        instrument: &Instrument,
        exchange: impl Into<String>,
        segment: impl Into<String>,
    ) -> Self {
        Self {
            key: instrument.key().to_owned(),
            exchange: exchange.into(),
            segment: segment.into(),
            kind: instrument.kind(),
            symbol: instrument.symbol().to_owned(),
            underlying: instrument.underlying().map(str::to_owned),
            expiry: instrument.expiry(),
            strike: instrument.strike(),
            lot_size: instrument.lot_size(),
            tick: instrument.tick_size(),
        }
    }
}

/// Lot-aware quantity model for Indian exchange display views.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct LotQuantity {
    pub lots: u64,
    pub lot_size: u64,
    pub total_units: u64,
}

impl LotQuantity {
    pub fn from_lots(lots: u64, lot_size: u64) -> Result<Self, FinancialError> {
        if lot_size == 0 {
            return Err(FinancialError::InvalidOrderGrid);
        }
        let total_units = lots
            .checked_mul(lot_size)
            .ok_or(FinancialError::InvalidOrderGrid)?;
        Ok(Self {
            lots,
            lot_size,
            total_units,
        })
    }

    pub fn from_units(units: u64, lot_size: u64) -> Result<Self, FinancialError> {
        if lot_size == 0 || !units.is_multiple_of(lot_size) {
            return Err(FinancialError::InvalidOrderGrid);
        }
        Ok(Self {
            lots: units / lot_size,
            lot_size,
            total_units: units,
        })
    }
}

/// Instrument margin representation for display views only.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct InstrumentMargin {
    pub instrument_key: String,
    pub span_margin: Option<crate::finance::Amount>,
    pub exposure_margin: Option<crate::finance::Amount>,
    pub total_margin: crate::finance::Amount,
    pub display_only: bool,
}

impl InstrumentMargin {
    pub fn new_display(
        instrument_key: impl Into<String>,
        span: Option<crate::finance::Amount>,
        exposure: Option<crate::finance::Amount>,
        total: crate::finance::Amount,
    ) -> Self {
        Self {
            instrument_key: instrument_key.into(),
            span_margin: span,
            exposure_margin: exposure,
            total_margin: total,
            display_only: true,
        }
    }
}

/// Strike row within an F&O option chain view.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FoChainStrike {
    pub strike: Price,
    pub call: Option<InstrumentRef>,
    pub put: Option<InstrumentRef>,
}

/// F&O chain view model for display only. Derivatives execution remains locked out.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct FoChainView {
    pub underlying: String,
    pub expiry: ExchangeDate,
    pub spot_price: Option<Price>,
    pub futures: Vec<InstrumentRef>,
    pub strikes: Vec<FoChainStrike>,
    pub display_only: bool,
}

impl FoChainView {
    pub fn new(
        underlying: impl Into<String>,
        expiry: ExchangeDate,
        spot_price: Option<Price>,
        futures: Vec<InstrumentRef>,
        strikes: Vec<FoChainStrike>,
    ) -> Self {
        Self {
            underlying: underlying.into(),
            expiry,
            spot_price,
            futures,
            strikes,
            display_only: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct QuoteFields {
    instrument: Instrument,
    bid: Price,
    ask: Price,
    bid_units: u64,
    ask_units: u64,
    source_at: Timestamp,
    received_at: Timestamp,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Quote(QuoteFields);

impl TryFrom<QuoteFields> for Quote {
    type Error = MarketError;
    fn try_from(value: QuoteFields) -> Result<Self, MarketError> {
        if value.bid > value.ask || value.source_at > value.received_at {
            return Err(MarketError::InvalidQuote);
        }
        Ok(Self(value))
    }
}
validated_record!(Quote, QuoteFields);

impl Quote {
    /// Construct a quote after source-specific parsers have checked provenance,
    /// freshness and price units. This enforces only the domain quote invariant.
    pub fn from_parts(
        instrument: Instrument,
        bid: Price,
        ask: Price,
        bid_units: u64,
        ask_units: u64,
        source_at: Timestamp,
        received_at: Timestamp,
    ) -> Result<Self, MarketError> {
        Self::try_from(QuoteFields {
            instrument,
            bid,
            ask,
            bid_units,
            ask_units,
            source_at,
            received_at,
        })
    }

    pub fn instrument(&self) -> &Instrument {
        &self.0.instrument
    }
    pub fn bid(&self) -> Price {
        self.0.bid
    }
    pub fn ask(&self) -> Price {
        self.0.ask
    }
    pub fn bid_units(&self) -> u64 {
        self.0.bid_units
    }
    pub fn ask_units(&self) -> u64 {
        self.0.ask_units
    }
    pub fn source_at(&self) -> Timestamp {
        self.0.source_at
    }
    pub fn received_at(&self) -> Timestamp {
        self.0.received_at
    }
    pub fn fresh(&self, now: Timestamp) -> bool {
        self.received_at() <= now
            && now
                .elapsed_micros_since(self.source_at())
                .is_ok_and(|age| (0..=MAX_FINANCIAL_AGE_MICROS).contains(&age))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct SnapshotFields {
    snapshot_id: String,
    captured_at: Timestamp,
    payload_hash: String,
    quotes: Vec<Quote>,
    source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct Snapshot(SnapshotFields);

impl TryFrom<SnapshotFields> for Snapshot {
    type Error = MarketError;
    fn try_from(value: SnapshotFields) -> Result<Self, MarketError> {
        let mut seen = BTreeSet::new();
        if value.quotes.iter().any(|quote| {
            !seen.insert(quote.instrument().key()) || quote.received_at() > value.captured_at
        }) {
            return Err(MarketError::InvalidSnapshot);
        }
        Ok(Self(value))
    }
}
validated_record!(Snapshot, SnapshotFields);

impl Snapshot {
    pub fn snapshot_id(&self) -> &str {
        &self.0.snapshot_id
    }
    pub fn captured_at(&self) -> Timestamp {
        self.0.captured_at
    }
    pub fn payload_hash(&self) -> &str {
        &self.0.payload_hash
    }
    pub fn quotes(&self) -> &[Quote] {
        &self.0.quotes
    }
    pub fn source(&self) -> &str {
        &self.0.source
    }

    /// Bind to a supplied archive projection. Actual body/hash rederivation and
    /// archive continuity are mandatory separate trusted-adapter obligations.
    pub fn bind_receipt(
        &self,
        receipt: &ArchivedQuoteReceipt,
        expected: &AccountSource,
    ) -> Result<ReceiptBoundSnapshot, MarketError> {
        if &receipt.0.identity != expected || self != &receipt.0.normalized_snapshot {
            return Err(MarketError::ReceiptMismatch);
        }
        Ok(ReceiptBoundSnapshot {
            snapshot: self.clone(),
            receipt: receipt.clone(),
        })
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ContinuousSessionFields {
    instrument_key: String,
    instrument_version: String,
    opening: LocalTime,
    closing: LocalTime,
    review_ref: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct ContinuousSession(ContinuousSessionFields);

impl TryFrom<ContinuousSessionFields> for ContinuousSession {
    type Error = MarketError;
    fn try_from(value: ContinuousSessionFields) -> Result<Self, MarketError> {
        if value.instrument_key.trim().is_empty()
            || value.instrument_key.chars().count() > 128
            || value.instrument_version.trim().is_empty()
            || value.review_ref.trim().is_empty()
            || value.review_ref.chars().count() > 256
            || value.opening >= value.closing
            || !value.opening.is_whole_minute()
            || !value.closing.is_whole_minute()
        {
            return Err(MarketError::InvalidContinuousSession);
        }
        Ok(Self(value))
    }
}
validated_record!(ContinuousSession, ContinuousSessionFields);

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CalendarFields {
    source: String,
    valid_from: ExchangeDate,
    valid_to: ExchangeDate,
    #[serde(default)]
    holidays: BTreeSet<ExchangeDate>,
    #[serde(default)]
    special: BTreeMap<ExchangeDate, (LocalTime, LocalTime)>,
    #[serde(default)]
    synthetic: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    continuous_session: Option<ContinuousSession>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct Calendar(CalendarFields);

impl TryFrom<CalendarFields> for Calendar {
    type Error = MarketError;
    fn try_from(value: CalendarFields) -> Result<Self, MarketError> {
        if value.source.is_empty()
            || value.valid_from > value.valid_to
            || value
                .holidays
                .iter()
                .chain(value.special.keys())
                .any(|day| *day < value.valid_from || *day > value.valid_to)
            || value
                .special
                .values()
                .any(|(opening, closing)| opening >= closing)
        {
            return Err(MarketError::InvalidCalendar);
        }
        Ok(Self(value))
    }
}
validated_record!(Calendar, CalendarFields);

impl Calendar {
    pub fn synthetic(&self) -> bool {
        self.0.synthetic
    }

    pub fn source(&self) -> &str {
        &self.0.source
    }

    pub fn valid_from(&self) -> ExchangeDate {
        self.0.valid_from
    }

    pub fn valid_to(&self) -> ExchangeDate {
        self.0.valid_to
    }

    pub fn hours(&self, day: ExchangeDate) -> Result<Option<(LocalTime, LocalTime)>, MarketError> {
        if day < self.0.valid_from || day > self.0.valid_to {
            return Err(MarketError::CalendarCoverageMissing);
        }
        if let Some(hours) = self.0.special.get(&day) {
            return Ok(Some(*hours));
        }
        if day.is_weekend() || self.0.holidays.contains(&day) {
            return Ok(None);
        }
        if let Some(binding) = &self.0.continuous_session {
            return Ok(Some((binding.0.opening, binding.0.closing)));
        }
        // Default cash session comes from the named NSE cash calendar, not an
        // anonymous constant. A reviewed calendar record still owns holidays.
        Ok(Some(SessionCalendarId::NseCash.hours()))
    }

    pub fn is_open(&self, now: Timestamp) -> Result<bool, MarketError> {
        let time = now.ist_time();
        Ok(self
            .hours(now.ist_date())?
            .is_some_and(|(opening, closing)| opening <= time && time < closing))
    }

    pub fn require_identity(&self, key: &str, version: &str) -> Result<(), MarketError> {
        if self.0.continuous_session.as_ref().is_some_and(|binding| {
            binding.0.instrument_key != key || binding.0.instrument_version != version
        }) {
            return Err(MarketError::InstrumentMismatch);
        }
        Ok(())
    }

    pub fn require_instrument(&self, instrument: &Instrument) -> Result<(), MarketError> {
        self.require_identity(instrument.key(), instrument.version())
    }

    pub fn require_key(&self, key: &str) -> Result<(), MarketError> {
        if self
            .0
            .continuous_session
            .as_ref()
            .is_some_and(|binding| binding.0.instrument_key != key)
        {
            return Err(MarketError::InstrumentMismatch);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct AccountSourceFields {
    broker: String,
    account_alias: String,
    archive_source: String,
    quote_source: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub struct AccountSource(AccountSourceFields);

impl TryFrom<AccountSourceFields> for AccountSource {
    type Error = MarketError;
    fn try_from(value: AccountSourceFields) -> Result<Self, MarketError> {
        if [
            &value.broker,
            &value.account_alias,
            &value.archive_source,
            &value.quote_source,
        ]
        .iter()
        .any(|field| field.trim().is_empty() || field.len() > 256)
        {
            return Err(MarketError::InvalidSourceIdentity);
        }
        Ok(Self(value))
    }
}
validated_record!(AccountSource, AccountSourceFields);

impl AccountSource {
    pub fn broker(&self) -> &str {
        &self.0.broker
    }
    pub fn archive_source(&self) -> &str {
        &self.0.archive_source
    }
    pub fn quote_source(&self) -> &str {
        &self.0.quote_source
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ArchivedQuoteReceiptFields {
    capture_id: String,
    identity: AccountSource,
    requested_at: Timestamp,
    received_at: Timestamp,
    normalized_snapshot: Snapshot,
}

#[derive(Debug, Clone, Serialize)]
#[serde(transparent)]
pub struct ArchivedQuoteReceipt(ArchivedQuoteReceiptFields);

impl ArchivedQuoteReceipt {
    pub fn snapshot(&self) -> &Snapshot {
        &self.0.normalized_snapshot
    }
}

impl TryFrom<ArchivedQuoteReceiptFields> for ArchivedQuoteReceipt {
    type Error = MarketError;
    fn try_from(value: ArchivedQuoteReceiptFields) -> Result<Self, MarketError> {
        let hash = |value: &str| {
            value.len() == 64
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        };
        if !hash(&value.capture_id)
            || !hash(value.normalized_snapshot.payload_hash())
            || value.normalized_snapshot.snapshot_id().is_empty()
            || value.requested_at > value.received_at
            || value.normalized_snapshot.captured_at() != value.received_at
            || value.normalized_snapshot.source() != value.identity.quote_source()
            || value.normalized_snapshot.quotes().is_empty()
            || value
                .normalized_snapshot
                .quotes()
                .iter()
                .any(|quote| quote.received_at() != value.received_at)
        {
            return Err(MarketError::InvalidReceipt);
        }
        Ok(Self(value))
    }
}
validated_record!(ArchivedQuoteReceipt, ArchivedQuoteReceiptFields);

#[derive(Debug, Clone)]
pub struct ReceiptBoundSnapshot {
    snapshot: Snapshot,
    receipt: ArchivedQuoteReceipt,
}

impl ReceiptBoundSnapshot {
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }
    pub fn received_at(&self) -> Timestamp {
        self.receipt.0.received_at
    }
    pub fn capture_id(&self) -> &str {
        &self.receipt.0.capture_id
    }

    pub fn check_fresh(&self, now: Timestamp) -> Result<(), MarketError> {
        if !now
            .elapsed_micros_since(self.received_at())
            .is_ok_and(|age| (0..=MAX_FINANCIAL_AGE_MICROS).contains(&age))
        {
            return Err(MarketError::FutureOrStaleReceipt);
        }
        if self.snapshot.quotes().iter().any(|quote| !quote.fresh(now)) {
            return Err(MarketError::FutureOrStaleQuote);
        }
        Ok(())
    }
}
