//! Snapshot reads with bounded BLOB allocation and one-chunk decoding.
use super::*;

diesel::define_sql_function! {
    #[sql_name = "length"]
    fn blob_length(bytes: diesel::sql_types::Binary) -> diesel::sql_types::BigInt;
}

#[derive(Queryable)]
struct ReadChunk {
    index: i64,
    offset: i64,
    length: i64,
    codec: String,
    digest: Vec<u8>,
    bytes: Option<Vec<u8>>,
}

impl Repository {
    #[cfg(test)]
    pub(super) fn read_page(&mut self, part: i64, offset: u64, length: usize) -> Result<Vec<u8>> {
        self.read_selected(Some(part), None, offset, length)
    }

    pub(super) fn read_authorized(
        &mut self,
        key: &RevisionKey,
        part: u32,
        size: u64,
        offset: u64,
        length: usize,
        active: bool,
    ) -> Result<Vec<u8>> {
        self.read_selected(None, Some((key, part, size, active)), offset, length)
    }

    fn read_selected(
        &mut self,
        part_id: Option<i64>,
        authorized: Option<(&RevisionKey, u32, u64, bool)>,
        offset: u64,
        length: usize,
    ) -> Result<Vec<u8>> {
        let offset = integer(offset)?;
        #[cfg(test)]
        let mut high_water = 0;
        let scratch = &mut self.read_scratch;
        let decoder = &mut self.read_decoder;
        #[cfg(test)]
        let after_lookup = self.after_read_lookup.take();
        let result = self.connection.transaction::<_, Error, _>(|c| {
            let mut query = parts::table
                .inner_join(revisions::table)
                .select((parts::id, parts::committed_raw_bytes, revisions::state))
                .into_boxed();
            if let Some(part) = part_id {
                query = query.filter(parts::id.eq(part));
            }
            if let Some((key, index, _, _)) = authorized {
                query = query
                    .filter(revisions::workspace_id.eq(&key.workspace_id))
                    .filter(revisions::conversation_id.eq(&key.conversation_id))
                    .filter(revisions::item_id.eq(&key.item_id))
                    .filter(revisions::payload_id.eq(&key.payload_id))
                    .filter(revisions::revision.eq(integer(key.revision)?))
                    .filter(parts::part_index.eq(i32::try_from(index)?));
            }
            let (part, size, state): (i64, i64, String) =
                query.first(c).optional()?.ok_or_else(|| {
                    std::io::Error::new(std::io::ErrorKind::NotFound, "payload part missing")
                })?;
            #[cfg(test)]
            if let Some(hook) = after_lookup {
                hook();
            }
            let active = authorized.is_some_and(|(_, _, _, active)| active);
            if authorized.is_some_and(|(_, _, expected, _)| {
                size < 0
                    || if active {
                        (size as u64) < expected
                    } else {
                        size as u64 != expected
                    }
            }) {
                return Err(invalid("descriptor length mismatch"));
            }
            let size = authorized.map_or(size, |(_, _, size, _)| size as i64);
            if (state != "ready" && !(active && state == "staging")) || offset > size {
                return Err(invalid("partial capture or invalid offset"));
            }
            let end = offset + (size - offset).min(length.min(PAGE) as i64);
            let mut output = Vec::with_capacity((end - offset) as usize);
            let mut position = offset;
            let mut index = None;
            while position < end {
                // CASE bounds corrupt BLOB allocation without substr's extra BLOB copy.
                let query = chunks::table.filter(chunks::part_id.eq(part)).select((
                    chunks::chunk_index,
                    chunks::raw_offset,
                    chunks::raw_length,
                    chunks::codec,
                    chunks::raw_sha256,
                    diesel::dsl::case_when(
                        blob_length(chunks::encoded_bytes).le(PAGE as i64),
                        chunks::encoded_bytes,
                    ),
                ));
                let row: ReadChunk = if let Some(index) = index {
                    query.filter(chunks::chunk_index.eq(index)).first(c)?
                } else {
                    // Locate and fetch the first chunk in one query.
                    query
                        .filter(chunks::raw_offset.le(position))
                        .order(chunks::raw_offset.desc())
                        .first(c)?
                };
                let bytes = row.bytes.ok_or_else(|| invalid("oversized chunk"))?;
                if row.length <= 0
                    || row.length > PAGE as i64
                    || row.offset > position
                    || row
                        .offset
                        .checked_add(row.length)
                        .is_none_or(|end| end <= position)
                    || (index.is_some() && row.offset != position)
                {
                    return Err(invalid("invalid chunk range"));
                }
                let decoded: &[u8] = match row.codec.as_str() {
                    "raw-v1" => &bytes,
                    "zstd-v1" => {
                        if decoder.is_none() {
                            let mut new = zstd::bulk::Decompressor::new()?;
                            new.set_parameter(zstd::zstd_safe::DParameter::WindowLogMax(16))?;
                            *decoder = Some(new);
                        }
                        scratch.resize(PAGE, 0);
                        let count = decoder
                            .as_mut()
                            .expect("initialized")
                            .decompress_to_buffer(&bytes, &mut scratch[..])?;
                        &scratch[..count]
                    }
                    _ => return Err(invalid("unknown codec")),
                };
                if decoded.len() != row.length as usize
                    || Sha256::digest(decoded).as_slice() != row.digest
                {
                    return Err(invalid("chunk checksum/length mismatch"));
                }
                let start = (position - row.offset) as usize;
                let take = (decoded.len() - start).min((end - position) as usize);
                output.extend_from_slice(&decoded[start..start + take]);
                #[cfg(test)]
                {
                    high_water = high_water.max(
                        output.capacity()
                            + bytes.capacity()
                            + scratch.capacity()
                            + row.digest.capacity(),
                    );
                }
                position += take as i64;
                index = Some(row.index + 1);
            }
            Ok(output)
        });
        #[cfg(test)]
        {
            self.metrics.read_buffers = self.metrics.read_buffers.max(high_water);
        }
        result
    }
}
