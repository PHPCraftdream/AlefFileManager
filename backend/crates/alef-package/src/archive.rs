// SPDX-License-Identifier: MIT OR Apache-2.0
//! Strict format-1 container parsing. No hash-list or signature interpretation.
use crate::{
    name::{comparison_key, validate_name, MAX_NAME_BYTES},
    PackageError,
};
use flate2::{Decompress, FlushDecompress, Status};
use std::{
    collections::HashSet,
    io::{Read, Seek, SeekFrom},
};

pub(crate) const MAX_ARCHIVE_SIZE: u64 = 2304 * 1024 * 1024;
pub(crate) const MAX_CENTRAL_SIZE: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_ENTRIES: usize = 10_000;
pub(crate) const MAX_ENTRY_SIZE: u64 = 512 * 1024 * 1024;
pub(crate) const MAX_TOTAL_SIZE: u64 = 2 * 1024 * 1024 * 1024;
pub(crate) const MAX_HASHES_SIZE: u64 = 16 * 1024 * 1024;
pub(crate) const MAX_SIGNATURE_SIZE: u64 = 256;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EntryMetadata {
    pub(crate) name: String,
    pub(crate) declared_size: u64,
    pub(crate) compressed_size: u64,
    pub(crate) method: u16,
    pub(crate) crc32: u32,
    data_offset: u64,
}

/// Owns the only parsed entry table and source. Payload reads return CRC-verified bytes.
pub(crate) struct Archive<R> {
    reader: R,
    entries: Vec<EntryMetadata>,
}
impl<R: Read + Seek> Archive<R> {
    pub(crate) fn entries(&self) -> &[EntryMetadata] {
        &self.entries
    }

    pub(crate) fn read_entry(&mut self, index: usize) -> Result<Vec<u8>, PackageError> {
        let entry = self
            .entries
            .get(index)
            .ok_or(invalid("entry index out of bounds"))?;
        read_payload(&mut self.reader, entry).map_err(|e| e.at_entry(&entry.name))
    }
}

fn invalid(reason: &'static str) -> PackageError {
    PackageError::InvalidArchive(reason)
}
fn add(a: u64, b: u64) -> Result<u64, PackageError> {
    a.checked_add(b).ok_or(PackageError::ArithmeticOverflow)
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16, PackageError> {
    let end = at.checked_add(2).ok_or(PackageError::ArithmeticOverflow)?;
    let raw = bytes.get(at..end).ok_or(invalid("truncated field"))?;
    Ok(u16::from_le_bytes([raw[0], raw[1]]))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32, PackageError> {
    let end = at.checked_add(4).ok_or(PackageError::ArithmeticOverflow)?;
    let raw = bytes.get(at..end).ok_or(invalid("truncated field"))?;
    Ok(u32::from_le_bytes([raw[0], raw[1], raw[2], raw[3]]))
}
fn allocated(size: u64) -> Result<Vec<u8>, PackageError> {
    let size = usize::try_from(size).map_err(|_| PackageError::ArithmeticOverflow)?;
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(size)
        .map_err(|_| PackageError::Allocation)?;
    bytes.resize(size, 0);
    Ok(bytes)
}
fn read_at<R: Read + Seek>(
    reader: &mut R,
    offset: u64,
    bytes: &mut [u8],
) -> Result<(), PackageError> {
    reader.seek(SeekFrom::Start(offset))?;
    reader.read_exact(bytes)?;
    Ok(())
}

pub(crate) fn inspect_archive<R: Read + Seek>(mut reader: R) -> Result<Archive<R>, PackageError> {
    let length = reader.seek(SeekFrom::End(0))?;
    if length > MAX_ARCHIVE_SIZE {
        return Err(PackageError::ArchiveSizeLimit);
    }
    let eocd_offset = length.checked_sub(22).ok_or(invalid("missing EOCD"))?;
    let mut end = [0; 22];
    read_at(&mut reader, eocd_offset, &mut end)?;
    if end[..4] != *b"PK\x05\x06" || u16_at(&end, 20)? != 0 {
        return Err(invalid("expected terminal 22-byte EOCD without comment"));
    }
    if u16_at(&end, 4)? != 0 || u16_at(&end, 6)? != 0 || u16_at(&end, 8)? != u16_at(&end, 10)? {
        return Err(PackageError::MultiDisk);
    }
    let count = usize::from(u16_at(&end, 10)?);
    let central_size = u32_at(&end, 12)?;
    let central_offset = u32_at(&end, 16)?;
    if count == usize::from(u16::MAX) || central_size == u32::MAX || central_offset == u32::MAX {
        return Err(PackageError::Zip64);
    }
    if count > MAX_ENTRIES {
        return Err(PackageError::EntryCountLimit);
    }
    if u64::from(central_size) > MAX_CENTRAL_SIZE {
        return Err(PackageError::CentralSizeLimit);
    }
    if add(u64::from(central_offset), u64::from(central_size))? != eocd_offset {
        return Err(invalid("central directory must end at EOCD"));
    }
    // Bound both the directory buffer and table before allocating either.
    if u64::from(central_size) < count as u64 * 46 {
        return Err(invalid("central count exceeds directory size"));
    }
    let mut central = allocated(u64::from(central_size))?;
    read_at(&mut reader, u64::from(central_offset), &mut central)?;
    let mut entries = Vec::new();
    entries
        .try_reserve_exact(count)
        .map_err(|_| PackageError::Allocation)?;
    let mut seen = HashSet::new();
    let mut cursor = 0usize;
    let mut total = 0u64;
    let mut local_offset = 0u64;
    for _ in 0..count {
        let fixed_end = cursor
            .checked_add(46)
            .ok_or(PackageError::ArithmeticOverflow)?;
        let header = central
            .get(cursor..fixed_end)
            .ok_or(invalid("truncated central header"))?;
        if header[..4] != *b"PK\x01\x02" {
            return Err(invalid("central signature"));
        }
        let name_len = usize::from(u16_at(header, 28)?);
        if name_len == 0 || name_len > MAX_NAME_BYTES {
            return Err(invalid("central name length outside 1..1024"));
        }
        let name_end = fixed_end
            .checked_add(name_len)
            .ok_or(PackageError::ArithmeticOverflow)?;
        let raw = central
            .get(fixed_end..name_end)
            .ok_or(invalid("truncated central name"))?;
        let name = validate_name(raw)?;
        let entry = (|| {
            if u16_at(header, 6)? > 20 {
                return Err(invalid("unsupported required ZIP version"));
            }
            if u16_at(header, 8)? != 0x0800 {
                return Err(invalid("flags must equal 0x0800"));
            }
            let method = u16_at(header, 10)?;
            if !matches!(method, 0 | 8) {
                return Err(PackageError::UnsupportedMethod(name.to_owned(), method));
            }
            if u16_at(header, 30)? != 0 || u16_at(header, 32)? != 0 {
                return Err(invalid("central extras and comments forbidden"));
            }
            if u16_at(header, 34)? != 0 {
                return Err(PackageError::MultiDisk);
            }
            let compressed = u32_at(header, 20)?;
            let size = u32_at(header, 24)?;
            let offset = u32_at(header, 42)?;
            if [compressed, size, offset].contains(&u32::MAX) {
                return Err(PackageError::Zip64);
            }
            let declared_size = u64::from(size);
            check_size(name, declared_size)?;
            total = add(total, declared_size)?;
            if total > MAX_TOTAL_SIZE {
                return Err(PackageError::TotalSizeLimit);
            }
            if method == 0 && compressed != size {
                return Err(invalid("stored sizes differ"));
            }
            if u64::from(offset) != local_offset {
                return Err(invalid(
                    "local records must be contiguous and in central order",
                ));
            }
            let fixed_local_end = add(local_offset, 30)?;
            let data_offset = add(fixed_local_end, name_len as u64)?;
            let data_end = add(data_offset, u64::from(compressed))?;
            if data_end > u64::from(central_offset) {
                return Err(invalid("local record overlaps central directory"));
            }
            let mut local = [0; 30];
            read_at(&mut reader, local_offset, &mut local)?;
            if local[..4] != *b"PK\x03\x04" {
                return Err(invalid("local signature"));
            }
            if u16_at(&local, 4)? > 20 || u16_at(&local, 28)? != 0 {
                return Err(invalid("local version or extra field"));
            }
            for (lp, cp, width) in [
                (4, 6, 2),
                (6, 8, 2),
                (8, 10, 2),
                (10, 12, 2),
                (12, 14, 2),
                (14, 16, 4),
                (18, 20, 4),
                (22, 24, 4),
                (26, 28, 2),
            ] {
                if local[lp..lp + width] != header[cp..cp + width] {
                    return Err(invalid("local/central metadata mismatch"));
                }
            }
            let mut local_name = [0; MAX_NAME_BYTES];
            read_at(&mut reader, fixed_local_end, &mut local_name[..name_len])?;
            if &local_name[..name_len] != raw {
                return Err(invalid("local/central name mismatch"));
            }
            local_offset = data_end;
            Ok(EntryMetadata {
                name: name.to_owned(),
                declared_size,
                compressed_size: u64::from(compressed),
                method,
                crc32: u32_at(header, 16)?,
                data_offset,
            })
        })()
        .map_err(|e: PackageError| e.at_entry(name))?;
        if !seen.insert(comparison_key(name)) {
            return Err(PackageError::DuplicateName(name.to_owned()));
        }
        entries.push(entry);
        cursor = name_end;
    }
    if cursor != central.len() || local_offset != u64::from(central_offset) {
        return Err(invalid("unaccounted records or gaps"));
    }
    for entry in &entries {
        let key = comparison_key(&entry.name);
        for (position, _) in key.match_indices('/') {
            if seen.contains(&key[..position]) {
                return Err(PackageError::PathConflict(entry.name.clone()));
            }
        }
    }
    if !seen.contains("alef.ktav") {
        return Err(PackageError::MissingManifest);
    }
    Ok(Archive { reader, entries })
}

fn check_size(name: &str, size: u64) -> Result<(), PackageError> {
    if size > MAX_ENTRY_SIZE {
        return Err(PackageError::EntrySizeLimit(name.to_owned()));
    }
    let limit = match name {
        "META-INF/hashes" => MAX_HASHES_SIZE,
        "META-INF/signature" => MAX_SIGNATURE_SIZE,
        _ => MAX_ENTRY_SIZE,
    };
    if size > limit {
        return Err(PackageError::MetadataSizeLimit(name.to_owned(), limit));
    }
    Ok(())
}

fn read_payload<R: Read + Seek>(
    reader: &mut R,
    entry: &EntryMetadata,
) -> Result<Vec<u8>, PackageError> {
    // Only validated metadata is admitted here; retain the limits before allocation.
    check_size(&entry.name, entry.declared_size)?;
    reader.seek(SeekFrom::Start(entry.data_offset))?;
    let mut output = allocated(add(entry.declared_size, 1)?)?;
    if entry.method == 0 {
        let n =
            usize::try_from(entry.declared_size).map_err(|_| PackageError::ArithmeticOverflow)?;
        reader.read_exact(&mut output[..n])?;
        output.truncate(n);
    } else {
        let mut decoder = Decompress::new(false);
        let mut input = [0; 64 * 1024];
        let mut remaining = entry.compressed_size;
        let mut start = 0usize;
        let mut end = 0usize;
        loop {
            if start == end && remaining != 0 {
                end = remaining.min(input.len() as u64) as usize;
                reader.read_exact(&mut input[..end])?;
                remaining -= end as u64;
                start = 0;
            }
            let before_in = decoder.total_in();
            let before_out = decoder.total_out();
            let out = usize::try_from(before_out).map_err(|_| PackageError::ArithmeticOverflow)?;
            let status = decoder
                .decompress(
                    &input[start..end],
                    &mut output[out..],
                    FlushDecompress::None,
                )
                .map_err(|_| PackageError::InvalidDeflate)?;
            start += usize::try_from(decoder.total_in() - before_in)
                .map_err(|_| PackageError::ArithmeticOverflow)?;
            if decoder.total_out() > entry.declared_size {
                return Err(PackageError::PayloadSizeMismatch);
            }
            if status == Status::StreamEnd {
                if decoder.total_in() != entry.compressed_size
                    || decoder.total_out() != entry.declared_size
                {
                    return Err(PackageError::PayloadSizeMismatch);
                }
                output.truncate(out + (decoder.total_out() - before_out) as usize);
                break;
            }
            if decoder.total_in() == before_in && decoder.total_out() == before_out {
                return Err(PackageError::InvalidDeflate);
            }
        }
    }
    if crc32fast::hash(&output) != entry.crc32 {
        return Err(PackageError::CrcMismatch);
    }
    Ok(output)
}

#[cfg(test)]
#[path = "archive_tests.rs"]
mod tests;
