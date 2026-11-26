use super::{ArchiveClient, Result};
use postgres::types::Type;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{collections::BTreeMap, fmt::Write};

const REFERENCE_KIND: &str = "market_archive_quote";
pub const MAX_QUOTE_BODY_BYTES: usize = 4_000_256;

#[derive(Deserialize, Serialize)]
struct Reference<'a> {
    kind: &'a str,
    purpose: &'a str,
    sha256: &'a str,
}

pub fn quote_reference(purpose: &str, raw: &str) -> String {
    let mut sha256 = String::with_capacity(64);
    for byte in Sha256::digest(raw.as_bytes()) {
        write!(sha256, "{byte:02x}").expect("write to String");
    }
    serde_json::to_string(&Reference {
        kind: REFERENCE_KIND,
        purpose,
        sha256: &sha256,
    })
    .expect("encode quote reference")
}

type ReceiptKey = ([u8; 32], i64);
type Bodies = BTreeMap<ReceiptKey, Vec<u8>>;

fn reference_key(raw: &str, received: Option<i64>) -> Result<ReceiptKey> {
    let reference: Reference<'_> =
        serde_json::from_str(raw).map_err(|_| "archive_quote_reference_invalid")?;
    if reference.kind != REFERENCE_KIND {
        return Err("archive_quote_reference_invalid".into());
    }
    let hex = reference.sha256;
    if hex.len() != 64 || !hex.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("archive_quote_reference_invalid".into());
    }
    let mut sha256 = [0; 32];
    for (index, byte) in sha256.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[index * 2..index * 2 + 2], 16)?;
    }
    Ok((
        sha256,
        received
            .filter(|ms| *ms > 0)
            .ok_or("archive_quote_reference_invalid")?,
    ))
}

fn fetch(db: &mut ArchiveClient, keys: &[ReceiptKey]) -> Result<Bodies> {
    let Some(from) = keys.iter().map(|key| key.1).min() else {
        return Ok(BTreeMap::new());
    };
    let to = keys
        .iter()
        .map(|key| key.1)
        .max()
        .expect("nonempty receipt keys");
    let hashes: Vec<&[u8]> = keys.iter().map(|(hash, _)| hash.as_slice()).collect();
    db.query(
        "SELECT sha256, received_ms, body FROM receipts
         WHERE kind = 1 AND received_ms BETWEEN $1 AND $2 AND sha256 = ANY($3)",
        &[&from, &to, &hashes],
    )?
    .into_iter()
    .map(|row| {
        let hash: [u8; 32] = row.get::<_, &[u8]>(0).try_into()?;
        Ok(((hash, row.get(1)), row.get(2)))
    })
    .collect()
}

fn decode((hash, _): ReceiptKey, compressed: &[u8]) -> Result<String> {
    let body = zstd::bulk::decompress(compressed, MAX_QUOTE_BODY_BYTES)?;
    if <[u8; 32]>::from(Sha256::digest(&body)) != hash {
        return Err("archive_receipt_conflict".into());
    }
    Ok(String::from_utf8(body)?)
}

/// A committed quote ID pins the visible prefix shared by capture and imports.
pub fn quote_watermark(db: &mut ArchiveClient) -> Result<i64> {
    Ok(db
        .query_one(
            "SELECT COALESCE((SELECT id FROM receipts WHERE kind = 1 ORDER BY id DESC LIMIT 1), 0)",
            &[],
        )?
        .get(0))
}

/// Select batches using the hypertable's receipt clock. Fetch bodies separately
/// so a session's entire compressed payload is never in memory at once.
pub fn quote_receipt_ids(
    db: &mut ArchiveClient,
    from_ms: i64,
    to_ms: i64,
    watermark: i64,
    limit: i64,
) -> Result<Vec<(i64, i64)>> {
    Ok(db
        .query_typed(
            "SELECT DISTINCT receipt_ms, receipt_id FROM quote_obs
             WHERE receipt_ms BETWEEN $1 AND $2 AND receipt_id <= $3
             AND purpose IN (1, 2, 3) AND source_ms <= receipt_ms
             ORDER BY receipt_ms, receipt_id LIMIT $4",
            &[
                (&from_ms, Type::INT8),
                (&to_ms, Type::INT8),
                (&watermark, Type::INT8),
                (&limit, Type::INT8),
            ],
        )?
        .iter()
        .map(|row| (row.get(0), row.get(1)))
        .collect())
}

pub fn quote_receipt_bodies(
    db: &mut ArchiveClient,
    receipts: &[(i64, i64)],
) -> Result<Vec<String>> {
    let ids: Vec<_> = receipts.iter().map(|&(_, id)| id).collect();
    let rows = db.query_typed(
        "SELECT r.received_ms, r.sha256, r.body
         FROM unnest($1::bigint[]) WITH ORDINALITY AS wanted(id, ord)
         JOIN receipts r USING (id) WHERE r.kind = 1 ORDER BY wanted.ord",
        &[(&ids, Type::INT8_ARRAY)],
    )?;
    if rows.len() != receipts.len() {
        return Err("archive_quote_body_missing".into());
    }
    rows.iter()
        .zip(receipts)
        .map(|(row, &(expected_ms, _))| {
            let received_ms = row.get(0);
            if received_ms != expected_ms {
                return Err("archive_receipt_conflict".into());
            }
            decode(
                (row.get::<_, &[u8]>(1).try_into()?, received_ms),
                row.get(2),
            )
        })
        .collect()
}

/// Resolve paper journal references in bounded pages with the original bytes.
#[derive(Default)]
pub struct QuoteBodies {
    db: Option<ArchiveClient>,
}

impl QuoteBodies {
    pub fn resolve_page(
        &mut self,
        rows: Vec<(Option<i64>, String)>,
        connect: impl FnOnce() -> Result<ArchiveClient>,
    ) -> Vec<Result<String>> {
        let refs: Vec<_> = rows
            .iter()
            .map(|(received, raw)| reference_key(raw, *received))
            .collect();
        let keys: Vec<_> = refs
            .iter()
            .filter_map(|key| key.as_ref().ok().copied())
            .collect();
        let bodies = (|| {
            if !keys.is_empty() && self.db.is_none() {
                self.db = Some(connect()?);
            }
            match &mut self.db {
                Some(db) => fetch(db, &keys),
                None => Ok(BTreeMap::new()),
            }
        })();
        refs.into_iter()
            .map(|key| {
                let key = key?;
                let compressed = bodies
                    .as_ref()
                    .map_err(|_| "archive_quote_read_failed")?
                    .get(&key)
                    .ok_or("archive_quote_body_missing")?;
                decode(key, compressed)
            })
            .collect()
    }

    pub fn resolve(
        &mut self,
        raw: String,
        receipt_ms: Option<i64>,
        connect: impl FnOnce() -> Result<ArchiveClient>,
    ) -> Result<String> {
        self.resolve_page(vec![(receipt_ms, raw)], connect)
            .pop()
            .expect("one receipt")
    }

    pub fn read(db: &mut ArchiveClient, raw: &str, receipt_ms: Option<i64>) -> Result<String> {
        let key = reference_key(raw, receipt_ms)?;
        decode(
            key,
            fetch(db, &[key])?
                .get(&key)
                .ok_or("archive_quote_body_missing")?,
        )
    }
}
