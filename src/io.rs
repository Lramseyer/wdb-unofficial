use std::io::{Read, Seek, SeekFrom};

use crate::error::{Result, WdbError};

pub fn read_u8<R: Read>(r: &mut R) -> Result<u8> {
    let mut buf = [0u8; 1];
    r.read_exact(&mut buf)?;
    Ok(buf[0])
}

pub fn read_u16_le<R: Read>(r: &mut R) -> Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf)?;
    Ok(u16::from_le_bytes(buf))
}

pub fn read_u32_le<R: Read>(r: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(u32::from_le_bytes(buf))
}

pub fn read_i32_le<R: Read>(r: &mut R) -> Result<i32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf)?;
    Ok(i32::from_le_bytes(buf))
}

pub fn read_u64_le<R: Read>(r: &mut R) -> Result<u64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(u64::from_le_bytes(buf))
}

pub fn read_i64_le<R: Read>(r: &mut R) -> Result<i64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(i64::from_le_bytes(buf))
}

pub fn read_f64_le<R: Read>(r: &mut R) -> Result<f64> {
    let mut buf = [0u8; 8];
    r.read_exact(&mut buf)?;
    Ok(f64::from_le_bytes(buf))
}

pub fn read_bytes<R: Read + ?Sized>(r: &mut R, n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    r.read_exact(&mut buf)?;
    Ok(buf)
}

/// Read a NUL-terminated string from a fixed-length field.
pub fn read_fixed_string<R: Read>(r: &mut R, len: usize) -> Result<String> {
    let buf = read_bytes(r, len)?;
    let s = buf
        .iter()
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    Ok(s)
}

/// Read a NUL-terminated string from a slice (no length prefix).
pub fn read_cstr(data: &[u8], offset: usize) -> &str {
    let end = data[offset..]
        .iter()
        .position(|&b| b == 0)
        .map(|p| offset + p)
        .unwrap_or(data.len());
    std::str::from_utf8(&data[offset..end]).unwrap_or("")
}

pub fn seek_to<S: Seek + ?Sized>(s: &mut S, offset: u64) -> Result<()> {
    s.seek(SeekFrom::Start(offset))?;
    Ok(())
}

pub fn skip<S: Seek>(s: &mut S, bytes: i64) -> Result<()> {
    s.seek(SeekFrom::Current(bytes))?;
    Ok(())
}

/// Read a little-endian u32 from a byte slice at a given offset.
pub fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// Read a little-endian i32 from a byte slice at a given offset.
pub fn i32_at(data: &[u8], offset: usize) -> i32 {
    i32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}

/// Read a little-endian u64 from a byte slice at a given offset.
pub fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

/// Read a little-endian i64 from a byte slice at a given offset.
pub fn i64_at(data: &[u8], offset: usize) -> i64 {
    i64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}

/// Validate a magic string read from a buffer.
pub fn check_magic(data: &[u8], expected: &str) -> Result<()> {
    let actual: String = data
        .iter()
        .take(expected.len())
        .take_while(|&&b| b != 0)
        .map(|&b| b as char)
        .collect();
    if actual != expected {
        return Err(WdbError::InvalidMagic {
            expected: expected.to_string(),
            got: actual,
        });
    }
    Ok(())
}
