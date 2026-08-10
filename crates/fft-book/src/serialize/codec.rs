use super::RestoreError;

pub(super) fn w8(bytes: &mut Vec<u8>, value: u8) {
    bytes.push(value);
}

pub(super) fn w16(bytes: &mut Vec<u8>, value: u16) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn w32(bytes: &mut Vec<u8>, value: u32) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn w64(bytes: &mut Vec<u8>, value: u64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn wi64(bytes: &mut Vec<u8>, value: i64) {
    bytes.extend_from_slice(&value.to_le_bytes());
}

pub(super) fn wopt_u64(bytes: &mut Vec<u8>, value: Option<u64>) {
    w8(bytes, u8::from(value.is_some()));
    w64(bytes, value.unwrap_or(0));
}

pub(super) fn wopt_i64(bytes: &mut Vec<u8>, value: Option<i64>) {
    w8(bytes, u8::from(value.is_some()));
    wi64(bytes, value.unwrap_or(0));
}

pub(super) struct Reader<'a> {
    bytes: &'a [u8],
    pos: usize,
    section: &'static str,
}

impl<'a> Reader<'a> {
    pub fn new(bytes: &'a [u8], section: &'static str) -> Self {
        Self {
            bytes,
            pos: 0,
            section,
        }
    }

    fn take(&mut self, len: usize) -> Result<&'a [u8], RestoreError> {
        let end = self.pos.checked_add(len).ok_or(RestoreError::Truncated {
            section: self.section,
        })?;
        if end > self.bytes.len() {
            return Err(RestoreError::Truncated {
                section: self.section,
            });
        }
        let out = &self.bytes[self.pos..end];
        self.pos = end;
        Ok(out)
    }

    pub fn u8(&mut self) -> Result<u8, RestoreError> {
        Ok(self.take(1)?[0])
    }

    pub fn u16(&mut self) -> Result<u16, RestoreError> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }

    pub fn u32(&mut self) -> Result<u32, RestoreError> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }

    pub fn u64(&mut self) -> Result<u64, RestoreError> {
        Ok(u64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn i64(&mut self) -> Result<i64, RestoreError> {
        Ok(i64::from_le_bytes(self.take(8)?.try_into().unwrap()))
    }

    pub fn boolean(&mut self) -> Result<bool, RestoreError> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(self.corrupt("invalid boolean")),
        }
    }

    pub fn opt_u64(&mut self) -> Result<Option<u64>, RestoreError> {
        let present = self.boolean()?;
        let value = self.u64()?;
        if !present && value != 0 {
            return Err(self.corrupt("nonzero absent optional value"));
        }
        Ok(present.then_some(value))
    }

    pub fn opt_i64(&mut self) -> Result<Option<i64>, RestoreError> {
        let present = self.boolean()?;
        let value = self.i64()?;
        if !present && value != 0 {
            return Err(self.corrupt("nonzero absent optional value"));
        }
        Ok(present.then_some(value))
    }

    pub fn count(&mut self, limit: u32) -> Result<u32, RestoreError> {
        let count = self.u32()?;
        if count > limit {
            return Err(self.corrupt("count exceeds limit"));
        }
        Ok(count)
    }

    pub fn count_with_min(
        &mut self,
        limit: u32,
        min_record_bytes: usize,
    ) -> Result<u32, RestoreError> {
        let count = self.count(limit)?;
        let minimum = usize::try_from(count)
            .ok()
            .and_then(|value| value.checked_mul(min_record_bytes))
            .ok_or_else(|| self.corrupt("count byte size overflow"))?;
        if minimum > self.bytes.len() - self.pos {
            return Err(RestoreError::Truncated {
                section: self.section,
            });
        }
        Ok(count)
    }

    pub fn finish(self) -> Result<(), RestoreError> {
        if self.pos == self.bytes.len() {
            Ok(())
        } else {
            Err(self.corrupt("trailing bytes"))
        }
    }

    pub fn corrupt(&self, what: &'static str) -> RestoreError {
        RestoreError::Corrupt {
            section: self.section,
            what,
        }
    }
}
